//! Which network adapter the phone can reach the PC on.
//!
//! The server binds to ONE address: the IPv4 of the real LAN adapter. [`pick`] chooses it from the
//! adapter list by the rules below (pure, tested with fake lists); [`list`] reads the real list with
//! `GetAdaptersAddresses`. The user can switch when there are several ([`candidates`]).
//!
//! An adapter qualifies when it is up, is Ethernet or Wi-Fi, has a **default gateway**, has a
//! private IPv4 address (10/8, 172.16/12, 192.168/16: not link-local, loopback or the carrier-grade
//! range VPNs like Tailscale use), and is not virtual or a VPN (Hyper-V / WSL / VMware /
//! VirtualBox / Docker / TAP / WireGuard / ..., judged by name and description). Wi-Fi comes
//! before Ethernet (the phone is on Wi-Fi), then the lower route metric.
//!
//! **A Public network is refused** (v0.3 fix): Windows asks "Public or Private?" for every new
//! network, and Public means "a network I do not trust" (a cafe, a hotel, an airport). Anything that
//! can reach the PC there could reach the transfer page too, so [`candidates`] leaves Public
//! adapters out unless the user pressed "Tetap lanjutkan" for this session (`allow_public`). The
//! category comes from `INetworkListManager` ([`network_categories`], read on a worker thread);
//! an adapter Windows has no category for ([`Category::Unknown`]) is not blocked.

use std::collections::HashMap;
use std::net::Ipv4Addr;

/// Windows' network category (`NLM_NETWORK_CATEGORY`) of the network an adapter is connected to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Category {
    /// "Public network": not trusted.
    Public,
    /// "Private network": the user's home or office.
    Private,
    /// A domain network (a managed company network).
    Domain,
    /// Not known (the Network List Manager did not answer, or knows no network for this adapter).
    #[default]
    Unknown,
}

impl Category {
    /// From the `NLM_NETWORK_CATEGORY` number: 0 public, 1 private, 2 domain authenticated.
    pub fn from_nlm(v: i32) -> Category {
        match v {
            0 => Category::Public,
            1 => Category::Private,
            2 => Category::Domain,
            _ => Category::Unknown,
        }
    }
}

/// `IF_TYPE_ETHERNET_CSMACD` and `IF_TYPE_IEEE80211`.
pub const IF_ETHERNET: u32 = 6;
pub const IF_WIFI: u32 = 71;

/// One IPv4 address of one adapter.
#[derive(Clone, Debug, PartialEq)]
pub struct Adapter {
    /// "Wi-Fi", "Ethernet", "vEthernet (WSL)".
    pub name: String,
    /// "Intel(R) Wi-Fi 6 AX201 160MHz".
    pub description: String,
    /// The adapter's GUID, lower case without braces (what the Network List Manager names it by).
    pub guid: String,
    /// Windows' category of the network it is on (Public networks are refused).
    pub category: Category,
    pub if_type: u32,
    pub up: bool,
    pub ip: Ipv4Addr,
    pub has_gateway: bool,
    /// IPv4 route metric (lower is preferred).
    pub metric: u32,
}

/// Words (or word prefixes) that mark a virtual adapter or a VPN. Short ones must match a whole
/// word (so "tun" does not match "tuning"), long ones match anywhere.
const VIRTUAL_WORDS: &[&str] = &["tun", "tap", "ppp", "wsl", "vpn", "hns", "vnic", "ras", "warp"];
const VIRTUAL_PARTS: &[&str] = &[
    "virtual",
    "vmware",
    "vmnet",
    "vethernet",
    "hyper-v",
    "virtualbox",
    "vbox",
    "docker",
    "wireguard",
    "wintun",
    "tailscale",
    "zerotier",
    "hamachi",
    "openvpn",
    "npcap",
    "loopback",
    "bluetooth",
    "pseudo",
    "teredo",
    "isatap",
    "6to4",
    "anyconnect",
    "fortinet",
    "nordlynx",
    "nordvpn",
    "proton",
    "cloudflare",
    "radmin",
    "sstp",
    "l2tp",
    "pptp",
    "miniport",
    "host-only",
    "kubernetes",
    "podman",
    "qemu",
    "parallels",
    "default switch",
    "tunnel",
    "wi-fi direct",
];

fn words(s: &str) -> impl Iterator<Item = String> + '_ {
    s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(|w| w.to_lowercase())
}

/// Does the name or description say "virtual adapter" or "VPN"?
pub fn looks_virtual(name: &str, description: &str) -> bool {
    let text = format!("{name} | {description}").to_lowercase();
    if VIRTUAL_PARTS.iter().any(|p| text.contains(p)) {
        return true;
    }
    // "tun0", "tap1", "ppp0": the number does not matter.
    words(&text).any(|w| VIRTUAL_WORDS.contains(&w.trim_end_matches(|c: char| c.is_ascii_digit())))
}

/// A private LAN address a phone on the same network can reach.
pub fn is_lan_ip(ip: Ipv4Addr) -> bool {
    // `is_private` is exactly 10/8, 172.16/12 and 192.168/16; link-local (169.254), loopback,
    // CGNAT (100.64/10, Tailscale) and public addresses are not.
    ip.is_private()
}

/// Can the phone use this adapter?
pub fn eligible(a: &Adapter) -> bool {
    a.up && matches!(a.if_type, IF_ETHERNET | IF_WIFI) && a.has_gateway && is_lan_ip(a.ip) && !looks_virtual(&a.name, &a.description)
}

/// Is this adapter on a network Windows calls Public?
pub fn is_public(a: &Adapter) -> bool {
    a.category == Category::Public
}

/// Indices of the usable adapters, best first: Wi-Fi before Ethernet, then the lower metric, then
/// the list order. An adapter on a Public network is left out unless `allow_public` (the user
/// confirmed it for this session).
pub fn candidates(list: &[Adapter], allow_public: bool) -> Vec<usize> {
    let mut v: Vec<usize> = (0..list.len()).filter(|&i| eligible(&list[i]) && (allow_public || !is_public(&list[i]))).collect();
    v.sort_by_key(|&i| (list[i].if_type != IF_WIFI, list[i].metric, i));
    v
}

/// The adapter to use: the user's earlier choice (by name) when it still qualifies, else the best.
pub fn pick(list: &[Adapter], prefer: Option<&str>, allow_public: bool) -> Option<usize> {
    let c = candidates(list, allow_public);
    prefer.and_then(|p| c.iter().copied().find(|&i| list[i].name == p)).or_else(|| c.first().copied())
}

/// True when the phone could be reached only over a Public network: a usable adapter exists, and
/// every one of them is Public. That is the case the island refuses and asks about.
pub fn only_public(list: &[Adapter]) -> bool {
    candidates(list, false).is_empty() && !candidates(list, true).is_empty()
}

/// Lower case, no braces: `{AB12...}` and `ab12...` name the same adapter.
pub fn norm_guid(s: &str) -> String {
    s.trim().trim_start_matches('{').trim_end_matches('}').to_lowercase()
}

/// What the card shows: "Wi-Fi · 192.168.1.23" (a sample address in screenshots, never in logs).
pub fn label(a: &Adapter) -> String {
    format!("{} · {}", a.name, a.ip)
}

// ---- the real list ----------------------------------------------------------------------------------

/// Windows' category of every connected network, by adapter GUID ([`norm_guid`]):
/// `INetworkListManager` -> `GetNetworkConnections` -> each connection's adapter and its network's
/// `GetCategory`. When an adapter has several connections the strictest category wins (Public).
/// Empty on any failure. Needs COM: call it on a worker thread (it starts its own apartment), never
/// on the UI thread.
pub fn network_categories() -> HashMap<String, Category> {
    use windows::Win32::Networking::NetworkListManager::{INetworkListManager, NetworkListManager};
    use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize};

    let mut map: HashMap<String, Category> = HashMap::new();
    unsafe {
        let init = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let mut read = || -> Option<()> {
            let nlm: INetworkListManager = CoCreateInstance(&NetworkListManager, None, CLSCTX_ALL).ok()?;
            let conns = nlm.GetNetworkConnections().ok()?;
            // Bounded: a misbehaving enumerator can not loop for ever.
            for _ in 0..128 {
                let mut slot = [None];
                let mut fetched = 0u32;
                if conns.Next(&mut slot, Some(&mut fetched)).is_err() || fetched == 0 {
                    break;
                }
                let Some(c) = slot[0].take() else { break };
                let (Ok(id), Ok(net)) = (c.GetAdapterId(), c.GetNetwork()) else { continue };
                let cat = net.GetCategory().map(|c| Category::from_nlm(c.0)).unwrap_or(Category::Unknown);
                let e = map.entry(norm_guid(&format!("{id:?}"))).or_insert(cat);
                if cat == Category::Public {
                    *e = Category::Public;
                }
            }
            Some(())
        };
        let _ = read();
        if init {
            CoUninitialize();
        }
    }
    map
}

/// [`list`] with the network category filled in ([`network_categories`]): for a worker thread.
pub fn list_with_categories() -> Vec<Adapter> {
    let mut l = list();
    let cats = network_categories();
    for a in &mut l {
        if let Some(c) = cats.get(&a.guid) {
            a.category = *c;
        }
    }
    l
}

/// The IPv4 addresses of this PC's adapters (`GetAdaptersAddresses`). Empty on any failure. The
/// category is [`Category::Unknown`] here: see [`list_with_categories`].
pub fn list() -> Vec<Adapter> {
    use windows::Win32::NetworkManagement::IpHelper::{
        GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH,
    };
    use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
    use windows::Win32::Networking::WinSock::AF_INET;

    const ERROR_BUFFER_OVERFLOW: u32 = 111;
    let flags = GAA_FLAG_INCLUDE_GATEWAYS | GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let mut size: u32 = 16 * 1024;
    // u64 elements: the structure needs 8-byte alignment.
    let mut buf: Vec<u64> = Vec::new();
    for _ in 0..4 {
        buf = vec![0u64; (size as usize).div_ceil(8)];
        let rc = unsafe { GetAdaptersAddresses(AF_INET.0 as u32, flags, None, Some(buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH), &mut size) };
        match rc {
            0 => break,
            ERROR_BUFFER_OVERFLOW => continue,
            _ => return vec![],
        }
    }
    let mut out = vec![];
    let mut cur = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
    // Bounded walk: a corrupt chain can not loop forever.
    for _ in 0..512 {
        if cur.is_null() {
            break;
        }
        let a = unsafe { &*cur };
        let name = unsafe { a.FriendlyName.to_string().unwrap_or_default() };
        let description = unsafe { a.Description.to_string().unwrap_or_default() };
        let guid = norm_guid(&unsafe { a.AdapterName.to_string().unwrap_or_default() });
        let has_gateway = !a.FirstGatewayAddress.is_null();
        let mut u = a.FirstUnicastAddress;
        for _ in 0..64 {
            if u.is_null() {
                break;
            }
            let ua = unsafe { &*u };
            let sa = ua.Address.lpSockaddr;
            // SOCKADDR_IN: family (2 bytes), port (2), address (4, network order).
            if !sa.is_null() && ua.Address.iSockaddrLength >= 8 && unsafe { (*sa).sa_family } == AF_INET {
                let p = sa as *const u8;
                let ip = unsafe { Ipv4Addr::new(*p.add(4), *p.add(5), *p.add(6), *p.add(7)) };
                out.push(Adapter {
                    name: name.clone(),
                    description: description.clone(),
                    guid: guid.clone(),
                    category: Category::Unknown,
                    if_type: a.IfType,
                    up: a.OperStatus == IfOperStatusUp,
                    ip,
                    has_gateway,
                    metric: a.Ipv4Metric,
                });
            }
            u = ua.Next;
        }
        cur = a.Next;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ad(name: &str, desc: &str, ty: u32, ip: [u8; 4], gw: bool, metric: u32) -> Adapter {
        Adapter { name: name.into(), description: desc.into(), guid: String::new(), category: Category::Private, if_type: ty, up: true, ip: Ipv4Addr::from(ip), has_gateway: gw, metric }
    }
    fn on(mut a: Adapter, c: Category) -> Adapter {
        a.category = c;
        a
    }
    fn wifi() -> Adapter {
        ad("Wi-Fi", "Intel(R) Wi-Fi 6 AX201 160MHz", IF_WIFI, [192, 168, 1, 23], true, 35)
    }
    fn ethernet() -> Adapter {
        ad("Ethernet", "Realtek PCIe GbE Family Controller", IF_ETHERNET, [192, 168, 1, 40], true, 25)
    }

    #[test]
    fn a_typical_laptop_with_virtual_adapters_picks_the_wifi() {
        let list = vec![
            ad("vEthernet (WSL (Hyper-V firewall))", "Hyper-V Virtual Ethernet Adapter", IF_ETHERNET, [172, 28, 16, 1], false, 5000),
            ad("vEthernet (Default Switch)", "Hyper-V Virtual Ethernet Adapter #2", IF_ETHERNET, [172, 22, 48, 1], false, 5000),
            ad("VMware Network Adapter VMnet8", "VMware Virtual Ethernet Adapter for VMnet8", IF_ETHERNET, [192, 168, 150, 1], false, 35),
            ad("Loopback Pseudo-Interface 1", "Software Loopback Interface 1", 24, [127, 0, 0, 1], false, 75),
            ad("Bluetooth Network Connection", "Bluetooth Device (Personal Area Network)", IF_ETHERNET, [169, 254, 7, 1], false, 65),
            wifi(),
        ];
        assert_eq!(pick(&list, None, false), Some(5));
        assert_eq!(candidates(&list, false), [5]);
    }

    #[test]
    fn a_vpn_with_a_gateway_and_a_private_address_is_still_skipped() {
        let list = vec![
            ad("Local Area Connection", "TAP-Windows Adapter V9", IF_ETHERNET, [10, 8, 0, 6], true, 1),
            ad("Tailscale", "Tailscale Tunnel", 131, [100, 101, 102, 103], true, 5),
            ad("WireGuard", "WireGuard Tunnel", 53, [10, 66, 66, 2], true, 5),
            ad("NordLynx", "NordLynx Tunnel", IF_ETHERNET, [10, 5, 0, 2], true, 1),
            ad("Proton VPN", "ProtonVPN TUN", IF_ETHERNET, [10, 96, 0, 2], true, 1),
            ad("Company VPN", "Cisco AnyConnect Secure Mobility Client Virtual Miniport Adapter", IF_ETHERNET, [10, 20, 30, 40], true, 1),
            ad("Ethernet 2", "Windscribe VPN", IF_ETHERNET, [10, 255, 0, 9], true, 1),
            ad("Dial-up", "WAN Miniport (PPTP)", 23, [10, 1, 1, 1], true, 1),
            ethernet(),
        ];
        assert_eq!(pick(&list, None, false), Some(8), "only the real Ethernet qualifies");
        assert_eq!(candidates(&list, false), [8]);
    }

    #[test]
    fn nothing_qualifies_gives_none() {
        let mut down = wifi();
        down.up = false;
        let mut no_gw = ethernet();
        no_gw.has_gateway = false;
        let link_local = ad("Wi-Fi 2", "Intel(R) Wi-Fi", IF_WIFI, [169, 254, 3, 4], true, 10);
        let public = ad("Ethernet 3", "Realtek", IF_ETHERNET, [8, 8, 4, 4], true, 10);
        let cgnat = ad("Ethernet 4", "Realtek", IF_ETHERNET, [100, 64, 1, 2], true, 10);
        assert_eq!(pick(&[down, no_gw, link_local, public, cgnat], None, false), None);
        assert_eq!(pick(&[], None, false), None);
    }

    #[test]
    fn wifi_before_ethernet_then_the_lower_metric() {
        // The phone is on Wi-Fi: it wins even though the cable has the better metric.
        let list = vec![ethernet(), wifi()];
        assert_eq!(candidates(&list, false), [1, 0]);
        assert_eq!(pick(&list, None, false), Some(1));
        // Two Wi-Fi adapters: the lower metric.
        let mut w2 = wifi();
        w2.name = "Wi-Fi 2".into();
        w2.ip = Ipv4Addr::new(10, 0, 0, 7);
        w2.metric = 20;
        let list = vec![wifi(), w2];
        assert_eq!(candidates(&list, false), [1, 0]);
        // Equal metric: list order.
        let mut a = wifi();
        let mut b = wifi();
        a.name = "A".into();
        b.name = "B".into();
        assert_eq!(candidates(&[a, b], false), [0, 1]);
    }

    #[test]
    fn the_users_choice_is_kept_while_it_still_qualifies() {
        let list = vec![wifi(), ethernet()];
        assert_eq!(pick(&list, Some("Ethernet"), false), Some(1));
        assert_eq!(pick(&list, Some("Wi-Fi"), false), Some(0));
        assert_eq!(pick(&list, Some("Gone"), false), Some(0), "an adapter that is no longer there falls back to the best");
        let mut e = ethernet();
        e.up = false;
        assert_eq!(pick(&[wifi(), e], Some("Ethernet"), false), Some(0), "a preferred adapter that is down is not used");
    }

    #[test]
    fn a_public_network_is_refused_unless_the_user_confirmed_this_session() {
        // Only a Public Wi-Fi: nothing is a candidate; the island asks.
        let cafe = vec![on(wifi(), Category::Public)];
        assert!(candidates(&cafe, false).is_empty());
        assert_eq!(pick(&cafe, None, false), None);
        assert!(only_public(&cafe));
        // "Tetap lanjutkan": allowed for that session.
        assert_eq!(candidates(&cafe, true), [0]);
        assert_eq!(pick(&cafe, None, true), Some(0));
        // A Private or Domain network, or one Windows has no category for, is fine.
        for c in [Category::Private, Category::Domain, Category::Unknown] {
            let l = vec![on(wifi(), c)];
            assert_eq!(pick(&l, None, false), Some(0), "{c:?}");
            assert!(!only_public(&l));
        }
        // Public Wi-Fi next to a Private cable: the cable is used, without asking.
        let both = vec![on(wifi(), Category::Public), on(ethernet(), Category::Private)];
        assert_eq!(candidates(&both, false), [1]);
        assert_eq!(pick(&both, Some("Wi-Fi"), false), Some(1), "even an earlier choice of the Public one");
        assert!(!only_public(&both));
        assert_eq!(candidates(&both, true), [0, 1], "confirmed: Wi-Fi first again");
        // Nothing usable at all is not "only public".
        assert!(!only_public(&[]));
        let mut down = on(wifi(), Category::Public);
        down.up = false;
        assert!(!only_public(&[down]));
    }

    #[test]
    fn categories_come_from_the_nlm_numbers_and_guids_compare_loosely() {
        assert_eq!(Category::from_nlm(0), Category::Public);
        assert_eq!(Category::from_nlm(1), Category::Private);
        assert_eq!(Category::from_nlm(2), Category::Domain);
        assert_eq!(Category::from_nlm(9), Category::Unknown);
        assert_eq!(norm_guid("{AB12CD34-0000-1111-2222-333344445555}"), "ab12cd34-0000-1111-2222-333344445555");
        assert_eq!(norm_guid(" ab12cd34-0000-1111-2222-333344445555 "), "ab12cd34-0000-1111-2222-333344445555");
    }

    #[test]
    fn the_real_network_categories_can_be_read() {
        // Asks Windows (read only); whatever it knows, every entry is a real category and the call returns.
        let cats = network_categories();
        for c in cats.values() {
            assert_ne!(*c, Category::Unknown);
        }
        let l = list_with_categories();
        for a in &l {
            assert!(!a.ip.is_unspecified());
        }
    }

    #[test]
    fn a_phone_hotspot_and_other_private_ranges_are_lan() {
        for ip in [[192, 168, 43, 5], [172, 20, 10, 2], [10, 0, 0, 2], [172, 16, 0, 1], [172, 31, 255, 254]] {
            assert!(is_lan_ip(Ipv4Addr::from(ip)), "{ip:?}");
        }
        for ip in [[172, 32, 0, 1], [172, 15, 0, 1], [169, 254, 1, 1], [127, 0, 0, 1], [100, 64, 0, 1], [0, 0, 0, 0], [224, 0, 0, 1], [8, 8, 8, 8]] {
            assert!(!is_lan_ip(Ipv4Addr::from(ip)), "{ip:?}");
        }
    }

    #[test]
    fn virtual_words_are_whole_words_where_they_are_short() {
        assert!(looks_virtual("Ethernet", "TAP-Windows Adapter V9"));
        assert!(looks_virtual("Local Area Connection* 10", "Microsoft Wi-Fi Direct Virtual Adapter"));
        assert!(looks_virtual("tun0", "something"));
        // Real hardware names that contain the letters but not the word.
        for (n, d) in [("Wi-Fi", "Intel(R) Wi-Fi 6E AX211 160MHz"), ("Ethernet", "Realtek USB GbE Family Controller"), ("Wi-Fi", "MediaTek Wi-Fi 6 MT7921 Wireless LAN Card"), ("Wi-Fi", "Qualcomm Atheros QCA9377 Wireless Network Adapter"), ("Ethernet", "Killer E2600 Gigabit Ethernet Controller"), ("Wi-Fi", "Broadcom 802.11ac Network Adapter"), ("Ethernet 3", "Remote NDIS based Internet Sharing Device")] {
            assert!(!looks_virtual(n, d), "{n} / {d}");
        }
    }

    #[test]
    fn the_real_adapter_list_can_be_read() {
        // Reads the machine's adapters; binds nothing. Every entry is a plausible address.
        let l = list();
        for a in &l {
            assert!(!a.ip.is_unspecified() && !a.ip.is_multicast(), "{a:?}");
        }
        let _ = pick(&l, None, false);
    }
}
