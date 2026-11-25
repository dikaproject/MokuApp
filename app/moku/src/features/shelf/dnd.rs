//! OLE drag and drop for the shelf: the drop target on the island's hit window, and
//! `DoDragDrop` for dragging a file back out. COM objects only; the shelf logic is in `mod.rs`.
//!
//! Both COM objects are hand-written static objects (a vtable and a no-op reference count),
//! because the `#[implement]` macro expands to `::windows_core`, which this crate does not depend
//! on directly (same approach as the volume OSD callbacks). They are stateless: the little state
//! they need lives in thread-locals of the UI thread, which is the thread OLE calls them on.

use std::cell::Cell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, E_INVALIDARG, E_NOINTERFACE, E_POINTER, HWND, POINTL, S_OK};
use windows::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, IDataObject, TYMED_HGLOBAL};
use windows::Win32::System::Ole::*;
use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP, ILClone, ILCreateFromPathW, ILFindLastID, ILFree, ILRemoveLastID, SHCreateDataObject};
use windows::core::{BOOL, GUID, HRESULT, HSTRING, IUnknown, IUnknown_Vtbl, Interface, PCWSTR, Result};

/// Never read more than this many names from one drop.
const MAX_DROPPED: u32 = 1000;

/// What the drop target reports to the shelf.
#[derive(Debug, PartialEq)]
pub enum DndEvent {
    /// A drag carrying files entered the island.
    Enter,
    /// It left again without dropping.
    Leave,
    /// Files were dropped.
    Drop(Vec<String>),
}

/// True while Moku itself is dragging a shelf file out: dropping it back onto the island must not
/// light up the drop state.
static SELF_DRAG: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// Where events go (set by [`register`]; tests set their own).
    static HANDLER: Cell<fn(DndEvent)> = const { Cell::new(|_| {}) };
    /// Is the shelf enabled in the settings?
    static ENABLED: Cell<fn() -> bool> = const { Cell::new(|| true) };
    /// The drag that is hovering carries files and we accept it.
    static ACCEPTING: Cell<bool> = const { Cell::new(false) };
}

fn hdrop_format() -> FORMATETC {
    FORMATETC { cfFormat: CF_HDROP.0, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0 as u32, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 }
}

/// Does this data object carry a file list?
pub fn has_files(data: &IDataObject) -> bool {
    unsafe { data.QueryGetData(&hdrop_format()) == S_OK }
}

/// The paths of a `CF_HDROP` payload (empty when there are none).
pub fn paths_from_data(data: &IDataObject) -> Vec<String> {
    let mut out = vec![];
    unsafe {
        let Ok(mut med) = data.GetData(&hdrop_format()) else { return out };
        let hdrop = HDROP(med.u.hGlobal.0);
        let n = DragQueryFileW(hdrop, 0xFFFF_FFFF, None).min(MAX_DROPPED);
        for i in 0..n {
            let len = DragQueryFileW(hdrop, i, None) as usize;
            if len == 0 {
                continue;
            }
            let mut buf = vec![0u16; len + 1];
            let got = DragQueryFileW(hdrop, i, Some(&mut buf)) as usize;
            out.push(String::from_utf16_lossy(&buf[..got.min(len)]));
        }
        ReleaseStgMedium(&mut med);
    }
    out
}

// ---- static COM plumbing ------------------------------------------------------------------------

#[repr(C)]
struct StaticCom<V: 'static> {
    vtbl: &'static V,
}
// SAFETY: immutable, only read by COM through the vtable pointer.
unsafe impl<V> Sync for StaticCom<V> {}

unsafe extern "system" fn add_ref(_this: *mut c_void) -> u32 {
    1
}
unsafe extern "system" fn release(_this: *mut c_void) -> u32 {
    1
}

unsafe fn query<I: Interface>(this: *mut c_void, iid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    if out.is_null() || iid.is_null() {
        return E_POINTER;
    }
    let iid = unsafe { *iid };
    if iid == I::IID || iid == IUnknown::IID {
        unsafe { *out = this };
        S_OK
    } else {
        unsafe { *out = std::ptr::null_mut() };
        E_NOINTERFACE
    }
}

fn com_ref<I: Interface, V>(obj: &'static StaticCom<V>) -> I {
    // SAFETY: `obj` starts with a vtable pointer of interface `I`; its reference count is a no-op.
    unsafe { I::from_raw(obj as *const StaticCom<V> as *mut c_void) }
}

// ---- drop target --------------------------------------------------------------------------------

unsafe extern "system" fn dt_qi(this: *mut c_void, iid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    unsafe { query::<IDropTarget>(this, iid, out) }
}

unsafe fn set_effect(p: *mut DROPEFFECT, e: DROPEFFECT) {
    if !p.is_null() {
        unsafe { *p = e };
    }
}

unsafe extern "system" fn dt_enter(_this: *mut c_void, data: *mut c_void, _keys: MODIFIERKEYS_FLAGS, _pt: POINTL, effect: *mut DROPEFFECT) -> HRESULT {
    let ok = ENABLED.with(|e| e.get())() && !SELF_DRAG.load(Ordering::SeqCst) && unsafe { IDataObject::from_raw_borrowed(&data) }.is_some_and(has_files);
    ACCEPTING.with(|a| a.set(ok));
    unsafe { set_effect(effect, if ok { DROPEFFECT_COPY } else { DROPEFFECT_NONE }) };
    if ok {
        HANDLER.with(|h| h.get())(DndEvent::Enter);
    }
    S_OK
}

unsafe extern "system" fn dt_over(_this: *mut c_void, _keys: MODIFIERKEYS_FLAGS, _pt: POINTL, effect: *mut DROPEFFECT) -> HRESULT {
    unsafe { set_effect(effect, if ACCEPTING.with(|a| a.get()) { DROPEFFECT_COPY } else { DROPEFFECT_NONE }) };
    S_OK
}

unsafe extern "system" fn dt_leave(_this: *mut c_void) -> HRESULT {
    if ACCEPTING.with(|a| a.replace(false)) {
        HANDLER.with(|h| h.get())(DndEvent::Leave);
    }
    S_OK
}

unsafe extern "system" fn dt_drop(_this: *mut c_void, data: *mut c_void, _keys: MODIFIERKEYS_FLAGS, _pt: POINTL, effect: *mut DROPEFFECT) -> HRESULT {
    let was = ACCEPTING.with(|a| a.replace(false));
    let paths = if was { unsafe { IDataObject::from_raw_borrowed(&data) }.map(paths_from_data).unwrap_or_default() } else { vec![] };
    // COPY only: the shelf never moves or deletes the user's files.
    unsafe { set_effect(effect, if paths.is_empty() { DROPEFFECT_NONE } else { DROPEFFECT_COPY }) };
    if was {
        HANDLER.with(|h| h.get())(DndEvent::Drop(paths));
    }
    S_OK
}

static TARGET_VTBL: IDropTarget_Vtbl = IDropTarget_Vtbl {
    base__: IUnknown_Vtbl { QueryInterface: dt_qi, AddRef: add_ref, Release: release },
    DragEnter: dt_enter,
    DragOver: dt_over,
    DragLeave: dt_leave,
    Drop: dt_drop,
};
static TARGET: StaticCom<IDropTarget_Vtbl> = StaticCom { vtbl: &TARGET_VTBL };

/// The drop target object (a static COM object: cloning and dropping it is free).
pub fn drop_target() -> IDropTarget {
    com_ref(&TARGET)
}

/// Make `hwnd` a drop target for files (UI thread, OLE initialised). `enabled` is asked on every
/// drag; events are delivered to `on_event` on the same thread.
pub fn register(hwnd: HWND, enabled: fn() -> bool, on_event: fn(DndEvent)) -> bool {
    ENABLED.with(|e| e.set(enabled));
    HANDLER.with(|h| h.set(on_event));
    match unsafe { RegisterDragDrop(hwnd, &drop_target()) } {
        Ok(()) => true,
        Err(e) => {
            log_error!("RegisterDragDrop failed: {e:?}");
            false
        }
    }
}

pub fn revoke(hwnd: HWND) {
    unsafe {
        let _ = RevokeDragDrop(hwnd);
    }
}

// ---- drag out ---------------------------------------------------------------------------------

unsafe extern "system" fn ds_qi(this: *mut c_void, iid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    unsafe { query::<IDropSource>(this, iid, out) }
}

unsafe extern "system" fn ds_query(_this: *mut c_void, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
    if escape.as_bool() {
        DRAGDROP_S_CANCEL
    } else if keys.0 & MK_LBUTTON.0 == 0 {
        DRAGDROP_S_DROP
    } else {
        S_OK
    }
}

unsafe extern "system" fn ds_feedback(_this: *mut c_void, _effect: DROPEFFECT) -> HRESULT {
    DRAGDROP_S_USEDEFAULTCURSORS
}

static SOURCE_VTBL: IDropSource_Vtbl = IDropSource_Vtbl {
    base__: IUnknown_Vtbl { QueryInterface: ds_qi, AddRef: add_ref, Release: release },
    QueryContinueDrag: ds_query,
    GiveFeedback: ds_feedback,
};
static SOURCE: StaticCom<IDropSource_Vtbl> = StaticCom { vtbl: &SOURCE_VTBL };

/// A standard shell data object for one file (CF_HDROP, shell ID list, file name …), so
/// Explorer, browsers and chat apps all accept it.
pub fn file_data_object(path: &str) -> Result<IDataObject> {
    unsafe {
        let wide = HSTRING::from(path);
        let abs = ILCreateFromPathW(PCWSTR(wide.as_ptr()));
        if abs.is_null() {
            return Err(E_INVALIDARG.into());
        }
        // Parent folder + child item: the form SHCreateDataObject documents.
        let child = ILFindLastID(abs);
        let parent = ILClone(abs);
        let _ = ILRemoveLastID(Some(parent));
        let r = SHCreateDataObject(Some(parent as *const _), Some(&[child as *const _]), None::<&IDataObject>);
        ILFree(Some(parent as *const _));
        ILFree(Some(abs as *const _));
        r
    }
}

/// Drag one file out of the island (modal: run it through `App::run_outside`). The drop effect is
/// COPY only, so whatever the target does, the original file stays where it is.
pub fn drag_out(path: &str) {
    let Ok(data) = file_data_object(path) else {
        log_warn!("shelf: cannot drag a file that does not exist any more");
        return;
    };
    let source: IDropSource = com_ref(&SOURCE);
    SELF_DRAG.store(true, Ordering::SeqCst);
    let mut effect = DROPEFFECT_NONE;
    unsafe {
        let _ = DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect);
    }
    SELF_DRAG.store(false, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};

    use super::*;

    thread_local! {
        static LOG: RefCell<Vec<DndEvent>> = const { RefCell::new(Vec::new()) };
    }

    fn record(e: DndEvent) {
        LOG.with(|l| l.borrow_mut().push(e));
    }

    fn take_log() -> Vec<DndEvent> {
        LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
    }

    fn temp_file(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("moku-shelf-dnd-{}-{}", std::process::id(), name.len()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join(name);
        std::fs::write(&f, b"data").unwrap();
        f
    }

    /// Drive the real IDropTarget (through its COM vtable) with a real shell data object, the
    /// kind Explorer hands over when files are dragged onto a window.
    #[test]
    fn shelf_drop_target_accepts_files_and_reads_their_paths() {
        unsafe {
            let _ = OleInitialize(None);
        }
        HANDLER.with(|h| h.set(record));
        ENABLED.with(|e| e.set(|| true));
        take_log();
        let f = temp_file("laporan Q4.txt");
        let path = f.to_string_lossy().to_string();
        let data = file_data_object(&path).expect("shell data object");
        assert!(has_files(&data));
        assert_eq!(paths_from_data(&data), vec![path.clone()]);

        let t = drop_target();
        let mut effect = DROPEFFECT_NONE;
        unsafe {
            t.DragEnter(&data, MODIFIERKEYS_FLAGS(0), POINTL::default(), &mut effect).unwrap();
            assert_eq!(effect, DROPEFFECT_COPY, "files are accepted as a copy (never a move)");
            effect = DROPEFFECT_NONE;
            t.DragOver(MODIFIERKEYS_FLAGS(0), POINTL::default(), &mut effect).unwrap();
            assert_eq!(effect, DROPEFFECT_COPY);
            t.Drop(&data, MODIFIERKEYS_FLAGS(0), POINTL::default(), &mut effect).unwrap();
            assert_eq!(effect, DROPEFFECT_COPY);
            // After the drop a stray DragLeave must not report a second "leave".
            t.DragLeave().unwrap();
        }
        assert_eq!(take_log(), vec![DndEvent::Enter, DndEvent::Drop(vec![path])]);
        assert!(f.exists(), "dropping never touches the file");

        // Enter, then leave without dropping.
        unsafe {
            t.DragEnter(&data, MODIFIERKEYS_FLAGS(0), POINTL::default(), &mut effect).unwrap();
            t.DragLeave().unwrap();
        }
        assert_eq!(take_log(), vec![DndEvent::Enter, DndEvent::Leave]);

        // The object answers QueryInterface for IDropTarget and IUnknown, and refuses the rest.
        assert!(t.cast::<IUnknown>().is_ok());
        assert!(t.cast::<IDataObject>().is_err());

        let _ = std::fs::remove_dir_all(f.parent().unwrap());
        unsafe {
            OleUninitialize();
        }
    }

    #[test]
    fn shelf_drop_target_refuses_when_disabled_and_during_its_own_drag() {
        unsafe {
            let _ = OleInitialize(None);
        }
        HANDLER.with(|h| h.set(record));
        take_log();
        let f = temp_file("x.txt");
        let data = file_data_object(&f.to_string_lossy()).unwrap();
        let t = drop_target();
        let mut effect = DROPEFFECT_COPY;

        ENABLED.with(|e| e.set(|| false));
        unsafe {
            t.DragEnter(&data, MODIFIERKEYS_FLAGS(0), POINTL::default(), &mut effect).unwrap();
            assert_eq!(effect, DROPEFFECT_NONE, "shelf disabled in settings: the island is not a drop target");
            t.Drop(&data, MODIFIERKEYS_FLAGS(0), POINTL::default(), &mut effect).unwrap();
        }
        assert!(take_log().is_empty());

        // Our own outgoing drag does not turn the island into a drop area.
        ENABLED.with(|e| e.set(|| true));
        SELF_DRAG.store(true, Ordering::SeqCst);
        unsafe {
            t.DragEnter(&data, MODIFIERKEYS_FLAGS(0), POINTL::default(), &mut effect).unwrap();
        }
        SELF_DRAG.store(false, Ordering::SeqCst);
        assert_eq!(effect, DROPEFFECT_NONE);
        assert!(take_log().is_empty());

        // Drag source: Esc cancels, releasing the button drops, otherwise keep going.
        let src: IDropSource = com_ref(&SOURCE);
        unsafe {
            assert_eq!(src.QueryContinueDrag(true, MODIFIERKEYS_FLAGS(MK_LBUTTON.0)), DRAGDROP_S_CANCEL);
            assert_eq!(src.QueryContinueDrag(false, MODIFIERKEYS_FLAGS(0)), DRAGDROP_S_DROP);
            assert_eq!(src.QueryContinueDrag(false, MODIFIERKEYS_FLAGS(MK_LBUTTON.0)), S_OK);
            assert_eq!(src.GiveFeedback(DROPEFFECT_COPY), DRAGDROP_S_USEDEFAULTCURSORS);
        }
        let _ = std::fs::remove_dir_all(f.parent().unwrap());
        unsafe {
            OleUninitialize();
        }
    }
}
