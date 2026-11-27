(() => {
  "use strict";
  const REPO = "dikaproject/MokuApp";
  const reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;
  const $ = (s, r = document) => r.querySelector(s);
  const island = $("#island");

  /* ---------- language ---------- */
  const root = document.documentElement;
  let stored = null;
  try { stored = localStorage.getItem("moku-lang"); } catch (_) {}
  const pick = stored || (/^id\b/i.test(navigator.language || "") ? "id" : "en");
  root.lang = pick === "en" ? "en" : "id";
  $("#lang").addEventListener("click", () => {
    root.lang = root.lang === "id" ? "en" : "id";
    try { localStorage.setItem("moku-lang", root.lang); } catch (_) {}
    renderRelease(release);
  });

  /* ---------- the island: one state at a time, driven by the scene in view ---------- */
  let state = "boot", seq = [];
  const clear = () => { seq.forEach(clearTimeout); seq = []; stopLyrics(); stopTimer(); };
  const later = (fn, ms) => seq.push(setTimeout(fn, ms));

  function setState(next) {
    if (next === state) return;
    state = next;
    island.dataset.state = next;
    clear();
    if (next === "panel") startLyrics();
    if (next === "timer") startTimer();
    if (next === "agent") agentLoop();
    if (next === "idle") tickClock();
  }

  function agentLoop() {
    const allow = $("#agent-allow");
    allow.classList.remove("is-pressed");
    if (reduced) return;
    later(() => allow.classList.add("is-pressed"), 2800);
    later(() => { island.dataset.state = "done"; state = "done"; }, 3300);
    later(() => { allow.classList.remove("is-pressed"); island.dataset.state = "agent"; state = "agent"; agentLoop(); }, 6200);
  }

  /* real local time in the idle pill, like the app */
  function tickClock() {
    const d = new Date();
    const hh = String(d.getHours()).padStart(2, "0"), mm = String(d.getMinutes()).padStart(2, "0");
    $(".st--idle .num").textContent = `${hh}.${mm}`;
  }
  setInterval(() => { if (state === "idle" || state === "polite") tickClock(); }, 15000);

  /* a focus timer that counts down while it is on show */
  let timerId = 0, left = 24 * 60 + 10;
  function startTimer() {
    const num = $(".st--timer .num"), arc = $(".ring__arc");
    const paint = () => {
      num.textContent = `${Math.floor(left / 60)}:${String(left % 60).padStart(2, "0")}`;
      arc.style.strokeDashoffset = String(100 - (left / (25 * 60)) * 100);
    };
    paint();
    if (reduced) return;
    timerId = setInterval(() => { left = left > 1 ? left - 1 : 25 * 60; paint(); }, 1000);
  }
  function stopTimer() { clearInterval(timerId); }

  /* word-by-word lyric sweep: the bright copy is revealed by an animated gradient edge */
  const LYRICS = [
    ["", "di bawah lampu jalan kita bicara", "tentang esok yang belum bernama"],
    ["di bawah lampu jalan kita bicara", "tentang esok yang belum bernama", "dan kota pun perlahan terlelap"],
    ["tentang esok yang belum bernama", "dan kota pun perlahan terlelap", "hanya kita yang masih menyala"],
    ["dan kota pun perlahan terlelap", "hanya kita yang masih menyala", ""],
  ];
  let lyricIdx = 0;
  function showLine(i) {
    const [prev, now, next] = LYRICS[i];
    const elNow = $("#lyr-now");
    $("#lyr-prev").textContent = prev || " ";
    $("#lyr-next").textContent = next || " ";
    elNow.classList.remove("is-run");
    elNow.textContent = now;
    elNow.style.setProperty("--dur", `${Math.max(2.2, now.length * .095)}s`);
    void elNow.offsetWidth;          // flush styles so the transition starts from the beginning
    elNow.classList.add("is-run");
    return Math.max(2.2, now.length * .095) * 1000 + 600;
  }
  function startLyrics() {
    const box = $("#lyr");
    box.classList.toggle("is-static", reduced);
    lyricIdx = 0;
    const step = () => {
      const wait = showLine(lyricIdx);
      lyricIdx = (lyricIdx + 1) % LYRICS.length;
      if (!reduced) later(step, wait);
    };
    step();
  }
  function stopLyrics() { $("#lyr-now").classList.remove("is-run"); }

  /* scene in view -> island state */
  const scenes = [...document.querySelectorAll("[data-island]")];
  const io = new IntersectionObserver((entries) => {
    entries.forEach((e) => {
      if (!e.isIntersecting) return;
      scenes.forEach((s) => s.classList.toggle("is-active", s === e.target));
      setState(e.target.dataset.island);
    });
  }, { rootMargin: "-46% 0px -46% 0px", threshold: 0 });
  scenes.forEach((s) => io.observe(s));

  /* the island wakes up once, then follows the page */
  tickClock();
  setTimeout(() => { if (state === "boot") { island.dataset.state = "idle"; state = "idle"; } }, reduced ? 0 : 450);

  /* ---------- download: the buttons work without JS (releases/latest/download); this adds version, size and checksum ---------- */
  let release = null;
  const fmtSize = (b) => (b / 1048576).toFixed(b > 10485760 ? 0 : 1) + " MB";
  function renderRelease(r) {
    if (!r) return;
    const id = root.lang === "id";
    const date = new Date(r.published_at).toLocaleDateString(id ? "id-ID" : "en-US", { day: "numeric", month: "long", year: "numeric" });
    const setup = r.assets.find((a) => a.name === "moku-setup.exe");
    const zip = r.assets.find((a) => /portable\.zip$/i.test(a.name));
    const size = setup ? ` · ${fmtSize(setup.size)}` : "";
    const line = id ? `Versi ${r.tag_name.replace(/^v/, "")}, dirilis ${date}${size}. Langsung dari GitHub Releases.`
                    : `Version ${r.tag_name.replace(/^v/, "")}, released ${date}${size}. Straight from GitHub Releases.`;
    $("#dl-meta").textContent = line;
    $("#dl-meta-2").textContent = line;
    if (zip) { const z = $("#dl-zip"); z.href = zip.browser_download_url; }
    const sha = setup && setup.digest && setup.digest.startsWith("sha256:") ? setup.digest.slice(7) : "";
    if (sha) { $("#sha-row").hidden = false; $("#sha-val").textContent = sha; }
  }
  async function loadRelease() {
    try {
      const cached = sessionStorage.getItem("moku-release");
      if (cached) { release = JSON.parse(cached); renderRelease(release); return; }
    } catch (_) {}
    try {
      const res = await fetch(`https://api.github.com/repos/${REPO}/releases/latest`, { headers: { Accept: "application/vnd.github+json" } });
      if (!res.ok) return;
      release = await res.json();
      try { sessionStorage.setItem("moku-release", JSON.stringify(release)); } catch (_) {}
      renderRelease(release);
    } catch (_) { /* offline or rate-limited: the static buttons still work */ }
  }
  loadRelease();

  $("#sha-copy").addEventListener("click", async (e) => {
    const btn = e.currentTarget, text = $("#sha-val").textContent;
    try { await navigator.clipboard.writeText(text); } catch (_) { return; }
    const old = btn.innerHTML;
    btn.textContent = root.lang === "id" ? "Tersalin" : "Copied";
    setTimeout(() => { btn.innerHTML = old; }, 1400);
  });
})();
