//! The one page the phone opens: written by hand, no framework, no external file. It talks to the
//! server with `fetch` / `XMLHttpRequest` on its own origin only (the content policy allows nothing
//! else), shows file names with `textContent` (never as markup), and says in the footer that the
//! connection is plain HTTP on the local network.
//!
//! Two shapes of the same page: **send** (the PC offers files: "Menunggu persetujuan di PC...", then
//! one download button per file) and **receive** (the phone picks files and/or types a text; the page
//! announces them, waits for the PC's "Terima", then uploads one file per request).

/// The page for a session. `send_files` is `Some(list of (name, size))` for a send session.
pub fn html(send_files: Option<&[(String, u64)]>) -> String {
    let (mode, files) = match send_files {
        Some(f) => ("send", json_for_script(&serde_json::json!(f.iter().map(|(n, s)| serde_json::json!({ "name": n, "size": s })).collect::<Vec<_>>()))),
        None => ("receive", "[]".to_string()),
    };
    TEMPLATE.replace("__MODE__", mode).replace("__FILES__", &files)
}

/// JSON that is safe inside a `<script>` element: no `</script>`, no HTML comment openers, no
/// line separators that end a JS string early.
fn json_for_script(v: &serde_json::Value) -> String {
    v.to_string().replace('<', "\\u003c").replace('>', "\\u003e").replace('&', "\\u0026").replace('\u{2028}', "\\u2028").replace('\u{2029}', "\\u2029")
}

const TEMPLATE: &str = r##"<!doctype html>
<html lang="id">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="color-scheme" content="dark light">
<title>Moku</title>
<style>
:root{--bg:#0d0f14;--card:#181b22;--fg:#f2f4f8;--dim:#9aa3b2;--acc:#0a84ff;--ok:#30d158;--bad:#ff453a;--line:#2a2f3a}
@media (prefers-color-scheme:light){:root{--bg:#f3f5f9;--card:#fff;--fg:#10131a;--dim:#5b6475;--line:#dde2ea}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:16px/1.45 system-ui,-apple-system,"Segoe UI",Roboto,sans-serif}
main{max-width:520px;margin:0 auto;padding:20px 16px 40px}
h1{font-size:22px;margin:8px 0 2px}
p.sub{margin:0 0 16px;color:var(--dim)}
.card{background:var(--card);border:1px solid var(--line);border-radius:18px;padding:14px;margin:12px 0}
.btn{display:block;width:100%;text-align:center;border:0;border-radius:14px;padding:14px 16px;font:inherit;font-weight:600;background:var(--acc);color:#fff;text-decoration:none}
.btn.alt{background:transparent;color:var(--fg);border:1px solid var(--line)}
.btn[disabled]{opacity:.45}
.row{display:flex;align-items:center;gap:10px;padding:10px 0;border-top:1px solid var(--line)}
.row:first-child{border-top:0}
.row .n{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.row .s{color:var(--dim);font-size:13px;white-space:nowrap}
.row .a{width:96px}
.row .a .btn{padding:8px 10px;font-size:14px}
.bar{height:6px;border-radius:3px;background:var(--line);overflow:hidden;margin-top:6px}
.bar i{display:block;height:100%;width:0;background:var(--acc)}
.bar.done i{background:var(--ok)}
.bar.fail i{background:var(--bad)}
textarea{width:100%;min-height:90px;border-radius:12px;border:1px solid var(--line);background:transparent;color:var(--fg);font:inherit;padding:10px}
#st{margin:12px 0;min-height:1.4em}
#st.bad{color:var(--bad)}
#st.ok{color:var(--ok)}
footer{margin-top:24px;color:var(--dim);font-size:12.5px}
input[type=file]{display:none}
</style>
</head>
<body>
<main>
<h1 id="h"></h1>
<p class="sub" id="sub"></p>
<div id="st"></div>
<div id="ui"></div>
<footer id="foot"></footer>
</main>
<script>
"use strict";
var MODE = "__MODE__", FILES = __FILES__;
var TXT = {
  id: {
    sendH: "Dari PC", sendSub: "File yang dikirim PC kamu.", recvH: "Kirim ke PC", recvSub: "Pilih file atau ketik teks untuk dikirim ke PC.",
    wait: "Menunggu persetujuan di PC\u2026", approved: "Disetujui.", denied: "PC menolak. Mulai lagi dari PC dengan kode QR baru.",
    ended: "Sesi berakhir. Mulai lagi dari PC.", other: "Perangkat lain sudah tersambung ke sesi ini.", lost: "Koneksi terputus. Cek Wi-Fi lalu coba lagi.",
    pick: "Pilih file", send: "Kirim", text: "Ketik atau tempel teks\u2026", download: "Unduh", done: "Selesai", fail: "Gagal", sending: "Mengirim\u2026",
    allDone: "Semua terkirim. Kamu bisa menutup halaman ini.", some: "Sebagian gagal.", none: "Pilih file atau ketik teks dulu.",
    foot: "Koneksi ini memakai HTTP biasa di jaringan Wi-Fi/LAN yang sama dengan PC. Jangan dipakai di Wi-Fi umum.", textSent: "Teks"
  },
  en: {
    sendH: "From the PC", sendSub: "The files your PC is sending.", recvH: "Send to the PC", recvSub: "Pick files or type a text to send to the PC.",
    wait: "Waiting for approval on the PC\u2026", approved: "Approved.", denied: "The PC declined. Start again on the PC with a new QR code.",
    ended: "The session ended. Start again on the PC.", other: "Another device is already connected to this session.", lost: "Connection lost. Check the Wi-Fi and try again.",
    pick: "Choose files", send: "Send", text: "Type or paste text\u2026", download: "Download", done: "Done", fail: "Failed", sending: "Sending\u2026",
    allDone: "Everything was sent. You can close this page.", some: "Some failed.", none: "Choose files or type a text first.",
    foot: "This connection is plain HTTP on the same Wi-Fi/LAN as the PC. Do not use it on public Wi-Fi.", textSent: "Text"
  }
};
var L = ((navigator.language || "id").toLowerCase().indexOf("en") === 0) ? TXT.en : TXT.id;
var $ = function (id) { return document.getElementById(id); };
function el(tag, cls, text) { var e = document.createElement(tag); if (cls) e.className = cls; if (text != null) e.textContent = text; return e; }
function size(n) { if (n < 1024) return n + " B"; if (n < 1048576) return Math.round(n / 1024) + " KB"; if (n < 1073741824) return (n / 1048576).toFixed(1) + " MB"; return (n / 1073741824).toFixed(1) + " GB"; }
function status(msg, cls) { var s = $("st"); s.textContent = msg || ""; s.className = cls || ""; }
$("foot").textContent = L.foot;

function api(path, body) {
  var o = { headers: { "X-Moku": "1" }, cache: "no-store" };
  if (body !== undefined) { o.method = "POST"; o.body = body; if (typeof body === "string") o.headers["Content-Type"] = "application/json"; }
  return fetch(path, o).then(function (r) { return r.json().catch(function () { return {}; }).then(function (j) { return { status: r.status, j: j }; }); });
}
function problem(r) {
  if (r.status === 403 && r.j.error === "device") return L.other;
  if (r.status === 410) return L.ended;
  return null;
}
function wait(ms) { return new Promise(function (ok) { setTimeout(ok, ms); }); }
// Poll until the PC answered. Resolves "allowed", or a message to show.
function approval() {
  return api("state").then(function (r) {
    var p = problem(r); if (p) return { err: p };
    var s = r.j.state;
    if (s === "allowed") return { ok: true };
    if (s === "denied") return { err: L.denied };
    if (s === "ended") return { err: L.ended };
    return wait(700).then(approval);
  });
}
function net() { status(L.lost, "bad"); }

function renderSend() {
  $("h").textContent = L.sendH; $("sub").textContent = L.sendSub;
  status(L.wait);
  api("offer", "{}").then(function (r) {
    var p = problem(r); if (p) return status(p, "bad");
    return approval().then(function (a) {
      if (a.err) return status(a.err, "bad");
      status(L.approved, "ok");
      var card = el("div", "card");
      FILES.forEach(function (f, i) {
        var row = el("div", "row");
        var n = el("div", "n", f.name); var s = el("div", "s", size(f.size));
        var a2 = el("div", "a"); var b = el("a", "btn", L.download); b.href = "f/" + i; b.setAttribute("download", f.name); a2.appendChild(b);
        row.appendChild(n); row.appendChild(s); row.appendChild(a2); card.appendChild(row);
      });
      $("ui").appendChild(card);
    });
  }).catch(net);
}

function renderReceive() {
  $("h").textContent = L.recvH; $("sub").textContent = L.recvSub;
  var picked = [];
  var ui = $("ui");
  var pick = el("label", "btn alt", L.pick);
  var inp = el("input"); inp.type = "file"; inp.multiple = true; pick.appendChild(inp);
  var list = el("div", "card"); list.style.display = "none";
  var ta = el("textarea"); ta.placeholder = L.text; ta.maxLength = 100000;
  var go = el("button", "btn", L.send); go.disabled = true;
  ui.appendChild(pick); ui.appendChild(list); ui.appendChild(el("div")).appendChild(ta); ui.lastChild.style.margin = "12px 0"; ui.appendChild(go);
  function refresh() { go.disabled = !(picked.length || ta.value.trim().length); }
  ta.addEventListener("input", refresh);
  inp.addEventListener("change", function () {
    picked = Array.prototype.slice.call(inp.files || []); list.textContent = ""; list.style.display = picked.length ? "" : "none";
    picked.forEach(function (f) {
      var row = el("div", "row"); var box = el("div", "n"); box.appendChild(el("div", "n", f.name)); var bar = el("div", "bar"); bar.appendChild(el("i")); box.appendChild(bar);
      row.appendChild(box); row.appendChild(el("div", "s", size(f.size))); list.appendChild(row); f._bar = bar;
    });
    refresh();
  });
  function upload(i, f) {
    return new Promise(function (ok) {
      var x = new XMLHttpRequest(); x.open("POST", "up/" + i); x.setRequestHeader("X-Moku", "1");
      x.upload.onprogress = function (e) { if (e.lengthComputable) f._bar.firstChild.style.width = Math.round(100 * e.loaded / e.total) + "%"; };
      x.onload = function () { var good = x.status === 200; f._bar.className = "bar " + (good ? "done" : "fail"); if (good) f._bar.firstChild.style.width = "100%"; ok(good); };
      x.onerror = x.ontimeout = x.onabort = function () { f._bar.className = "bar fail"; ok(false); };
      var fd = new FormData(); fd.append("file", f, f.name); x.send(fd);
    });
  }
  go.addEventListener("click", function () {
    var txt = ta.value.trim();
    if (!picked.length && !txt) return status(L.none, "bad");
    go.disabled = true; pick.style.pointerEvents = "none"; status(L.wait);
    var offer = JSON.stringify({ files: picked.map(function (f) { return { name: f.name, size: f.size }; }), text: txt.length > 0 });
    api("offer", offer).then(function (r) {
      var p = problem(r); if (p) return status(p, "bad");
      if (r.status !== 200) return status(L.fail, "bad");
      return approval().then(function (a) {
        if (a.err) return status(a.err, "bad");
        status(L.sending);
        var okAll = true, chain = Promise.resolve();
        picked.forEach(function (f, i) { chain = chain.then(function () { return upload(i, f); }).then(function (g) { if (!g) okAll = false; }); });
        return chain.then(function () {
          if (!txt) return;
          return api("text", txt).then(function (r2) { if (r2.status !== 200) okAll = false; });
        }).then(function () { status(okAll ? L.allDone : L.some, okAll ? "ok" : "bad"); });
      });
    }).catch(net);
  });
}
if (MODE === "send") renderSend(); else renderReceive();
</script>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_shapes_differ_only_in_mode_and_files() {
        let recv = html(None);
        assert!(recv.contains("var MODE = \"receive\", FILES = [];"));
        let send = html(Some(&[("a.pdf".into(), 1234)]));
        assert!(send.contains("var MODE = \"send\", FILES = [{\"name\":\"a.pdf\",\"size\":1234}];"));
        assert!(!recv.contains("__MODE__") && !recv.contains("__FILES__") && !send.contains("__FILES__"));
    }

    #[test]
    fn file_names_cannot_break_out_of_the_script() {
        let nasty = "</script><script>alert(1)</script>\u{2028}&<!--\"\\";
        let page = html(Some(&[(nasty.into(), 1)]));
        // Exactly the page's own script element opens and closes.
        assert_eq!(page.matches("<script>").count(), 1);
        assert_eq!(page.matches("</script>").count(), 1);
        assert!(!page.contains('\u{2028}'));
        assert!(page.contains("\\u003c/script\\u003e"));
        // And the data survives the JSON round trip.
        let start = page.find("FILES = ").unwrap() + 8;
        let end = page[start..].find(";\n").unwrap() + start;
        let parsed: serde_json::Value = serde_json::from_str(&page[start..end]).unwrap();
        assert_eq!(parsed[0]["name"].as_str().unwrap(), nasty);
    }

    #[test]
    fn the_page_is_self_contained_and_never_builds_markup_from_data() {
        let page = html(None);
        // Nothing is loaded from anywhere else: no external URL in an attribute or import.
        for bad in ["src=\"http", "href=\"http", "@import", "url(http", "<link", "<iframe", "<img", "<form"] {
            assert!(!page.contains(bad), "{bad}");
        }
        // Names and texts are shown with textContent; innerHTML / document.write are never used.
        for bad in ["innerHTML", "outerHTML", "document.write", "eval(", "insertAdjacentHTML"] {
            assert!(!page.contains(bad), "{bad}");
        }
        // The plain-HTTP note is on the page in both languages.
        assert!(page.contains("HTTP biasa") && page.contains("plain HTTP"));
        // Every request carries the header the server demands.
        assert!(page.contains("X-Moku"));
    }
}
