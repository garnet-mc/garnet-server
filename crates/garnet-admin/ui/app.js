// Garnet admin panel. Plain JavaScript, no build step: the server embeds
// this file and serves it as-is.

const main = document.getElementById("main");
const sidebar = document.getElementById("sidebar");
const ROLE_RANK = { viewer: 0, moderator: 1, admin: 2, owner: 3 };
let me = null;
let consoleSocket = null;
let tpsHistory = [];
let playersHistory = [];

// ---------- helpers ----------

async function api(method, path, body) {
  const options = { method, headers: {} };
  if (body !== undefined) {
    if (typeof body === "string") {
      options.headers["Content-Type"] = "text/plain";
      options.body = body;
    } else {
      options.headers["Content-Type"] = "application/json";
      options.body = JSON.stringify(body);
    }
  }
  const response = await fetch(path, options);
  let data = null;
  try { data = await response.json(); } catch (_) { data = null; }
  if (!response.ok) {
    const message = (data && data.error) || response.statusText;
    if (response.status === 401 && me) { me = null; render(); }
    throw new Error(message);
  }
  return data;
}

function toast(message, isError) {
  const el = document.getElementById("toast");
  el.textContent = message;
  el.className = isError ? "err" : "";
  el.hidden = false;
  clearTimeout(el._timer);
  el._timer = setTimeout(() => { el.hidden = true; }, 3500);
}

function esc(s) {
  return String(s ?? "").replace(/[&<>"']/g, c => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

function hasRole(role) {
  return me && ROLE_RANK[me.role] >= ROLE_RANK[role];
}

function fmtDuration(seconds) {
  const d = Math.floor(seconds / 86400), h = Math.floor(seconds % 86400 / 3600), m = Math.floor(seconds % 3600 / 60);
  if (d) return `${d}d ${h}h`;
  if (h) return `${h}h ${m}m`;
  return `${m}m ${Math.floor(seconds % 60)}s`;
}

function fmtTime(ticks) {
  const t = ((ticks % 24000) + 24000) % 24000;
  const hours = Math.floor((t + 6000) / 1000) % 24;
  const minutes = Math.floor(((t + 6000) % 1000) / 1000 * 60);
  return `${String(hours).padStart(2, "0")}:${String(minutes).padStart(2, "0")}`;
}

function sparkline(canvas, values, max, color) {
  const ctx = canvas.getContext("2d");
  const w = canvas.width = canvas.clientWidth * devicePixelRatio;
  const h = canvas.height = canvas.clientHeight * devicePixelRatio;
  ctx.clearRect(0, 0, w, h);
  if (values.length < 2) return;
  ctx.strokeStyle = color;
  ctx.lineWidth = 2 * devicePixelRatio;
  ctx.beginPath();
  values.forEach((v, i) => {
    const x = i / (values.length - 1) * w;
    const y = h - Math.min(v / max, 1) * (h - 6) - 3;
    i ? ctx.lineTo(x, y) : ctx.moveTo(x, y);
  });
  ctx.stroke();
}

// ---------- pages ----------

const pages = {};

pages.login = async () => {
  const setup = await api("GET", "/api/setup/status" + location.search).catch(() => ({ needs_setup: false }));
  const token = new URLSearchParams(location.search).get("token");
  if (setup.needs_setup) {
    main.innerHTML = `
      <div class="login">
        <img src="/logo.png" alt="">
        <h1>Set up Garnet</h1>
        ${setup.token_valid ? `
          <p class="help">Create the owner account. This is the master admin for the panel and does not need to be an in-game op.</p>
          <label>Username</label><input type="text" id="u" autocomplete="username">
          <label>Password (8+ characters)</label><input type="password" id="p" autocomplete="new-password">
          <button class="btn primary" id="go">Create owner account</button>
          <div class="error" id="err"></div>`
        : `<p class="help">No accounts exist yet. Open the setup link printed in the server console (it starts with <code>/setup?token=</code>). Restart the server if it expired.</p>`}
      </div>`;
    const go = document.getElementById("go");
    if (go) go.onclick = async () => {
      try {
        await api("POST", "/api/setup", { token, username: u.value, password: p.value });
        history.replaceState(null, "", "/");
        location.hash = "#dashboard";
        await boot();
      } catch (e) { document.getElementById("err").textContent = e.message; }
    };
    return;
  }
  main.innerHTML = `
    <div class="login">
      <img src="/logo.png" alt="">
      <h1>Garnet</h1>
      <label>Username</label><input type="text" id="u" autocomplete="username">
      <label>Password</label><input type="password" id="p" autocomplete="current-password">
      <button class="btn primary" id="go">Log in</button>
      <div class="error" id="err"></div>
      <p class="help">Opped players can also type <code>/panel</code> in game for a one-time login link.</p>
    </div>`;
  const submit = async () => {
    try {
      await api("POST", "/api/login", { username: u.value, password: p.value });
      await boot();
    } catch (e) { document.getElementById("err").textContent = e.message; }
  };
  document.getElementById("go").onclick = submit;
  main.querySelectorAll("input").forEach(i => i.onkeydown = e => { if (e.key === "Enter") submit(); });
};

pages.dashboard = async () => {
  const s = await api("GET", "/api/status");
  document.getElementById("server-name").textContent = s.name;
  tpsHistory.push(s.tps); if (tpsHistory.length > 120) tpsHistory.shift();
  playersHistory.push(s.online); if (playersHistory.length > 120) playersHistory.shift();
  main.innerHTML = `
    <h1>Dashboard</h1>
    <div class="cards">
      <div class="card"><div class="label">Players</div><div class="value">${s.online} <small>/ ${s.max_players}</small></div></div>
      <div class="card"><div class="label">TPS</div><div class="value">${s.tps.toFixed(1)} <small>${s.tick_ms.toFixed(1)} ms/tick</small></div></div>
      <div class="card"><div class="label">Uptime</div><div class="value">${fmtDuration(s.uptime_seconds)}</div></div>
      <div class="card"><div class="label">Memory</div><div class="value">${s.memory_mb} <small>MB</small></div></div>
      <div class="card"><div class="label">Loaded chunks</div><div class="value">${s.loaded_chunks}</div></div>
      <div class="card"><div class="label">World time</div><div class="value">${fmtTime(s.world_time)}</div></div>
      <div class="card"><div class="label">Voice</div><div class="value">${s.voice_connected} <small>connected</small></div></div>
      <div class="card"><div class="label">Minecraft</div><div class="value">${esc(s.minecraft_version)} <small>protocol ${s.protocol_version}</small></div></div>
    </div>
    <div class="grid-2" style="margin-top:12px">
      <div class="card"><div class="label">TPS (last ${tpsHistory.length * 3}s)</div><canvas class="spark" id="tps"></canvas></div>
      <div class="card"><div class="label">Players online</div><canvas class="spark" id="pl"></canvas></div>
    </div>
    <h2>Server</h2>
    <div class="row">
      <span class="pill ${s.online_mode ? "on" : "warn"}">${s.online_mode ? "online mode" : "offline mode"}</span>
      <span class="pill">Garnet ${esc(s.version)}</span>
      <span class="pill">world: ${esc(s.world_name)}</span>
      <span class="spacer"></span>
      ${hasRole("admin") ? `<button class="btn" id="save">Save world</button><button class="btn" id="backup">Backup now</button>` : ""}
      ${hasRole("owner") ? `<button class="btn danger" id="stop">Stop server</button>` : ""}
    </div>`;
  sparkline(document.getElementById("tps"), tpsHistory, 20, "#4cd37a");
  sparkline(document.getElementById("pl"), playersHistory, Math.max(s.max_players, 1), "#ff4d6d");
  const bind = (id, fn) => { const el = document.getElementById(id); if (el) el.onclick = fn; };
  bind("save", async () => { await api("POST", "/api/save"); toast("World saved"); });
  bind("backup", async () => { toast("Backing up..."); await api("POST", "/api/backups"); toast("Backup created"); });
  bind("stop", async () => { if (confirm("Stop the server?")) { await api("POST", "/api/stop"); toast("Stopping"); } });
};

pages.players = async () => {
  const list = await api("GET", "/api/players");
  main.innerHTML = `
    <h1>Players <span class="muted">(${list.length})</span></h1>
    ${hasRole("moderator") ? `<div class="row" style="margin-bottom:12px"><input type="text" id="bc" placeholder="Broadcast a message to everyone" style="flex:1"><button class="btn" id="bcgo">Send</button></div>` : ""}
    <table><thead><tr><th>Name</th><th>Mode</th><th>Ping</th><th>Position</th><th>Online</th><th>Flags</th><th></th></tr></thead>
    <tbody>${list.map(p => `<tr>
      <td><b>${esc(p.name)}</b><div class="muted mono">${p.uuid}</div>${hasRole("moderator") ? `<div class="muted mono">${esc(p.ip)}</div>` : ""}</td>
      <td>${esc(p.game_mode)}</td>
      <td>${p.latency_ms} ms</td>
      <td class="mono">${p.x.toFixed(0)}, ${p.y.toFixed(0)}, ${p.z.toFixed(0)}<div class="muted">${esc(p.dimension)}</div></td>
      <td>${fmtDuration(p.online_seconds)}</td>
      <td>${p.is_op ? '<span class="pill on">op</span> ' : ""}${p.voice_connected ? '<span class="pill">voice</span> ' : ""}${p.violations ? `<span class="pill warn">${p.violations} flags</span>` : ""}</td>
      <td class="actions">
        ${hasRole("moderator") ? `<button class="btn small" data-act="msg" data-id="${p.uuid}">Message</button>
        <button class="btn small" data-act="mute" data-id="${p.uuid}">Mute voice</button>
        <button class="btn small" data-act="kick" data-id="${p.uuid}">Kick</button>
        <button class="btn small danger" data-act="ban" data-id="${p.uuid}">Ban</button>` : ""}
        ${hasRole("admin") ? `<select class="small" data-act="mode" data-id="${p.uuid}">
          ${["survival", "creative", "adventure", "spectator"].map(m => `<option ${m === p.game_mode ? "selected" : ""}>${m}</option>`).join("")}</select>
        <button class="btn small" data-act="${p.is_op ? "deop" : "op"}" data-id="${p.uuid}">${p.is_op ? "De-op" : "Op"}</button>` : ""}
      </td></tr>`).join("") || `<tr><td colspan="7" class="muted">Nobody online.</td></tr>`}</tbody></table>`;
  const bcgo = document.getElementById("bcgo");
  if (bcgo) bcgo.onclick = async () => { await api("POST", "/api/broadcast", { text: bc.value }); bc.value = ""; toast("Sent"); };
  main.querySelectorAll("[data-act]").forEach(el => {
    const id = el.dataset.id, act = el.dataset.act;
    const handler = async () => {
      try {
        if (act === "kick") { const reason = prompt("Kick reason", "Kicked by an admin"); if (reason === null) return; await api("POST", `/api/players/${id}/kick`, { reason }); }
        if (act === "ban") { const reason = prompt("Ban reason", "Banned by an admin"); if (reason === null) return; const h = prompt("Duration in hours (blank = permanent)", ""); await api("POST", `/api/players/${id}/ban`, { reason, hours: h ? Number(h) : null }); }
        if (act === "msg") { const text = prompt("Message"); if (!text) return; await api("POST", `/api/players/${id}/message`, { text }); }
        if (act === "mute") { await api("POST", `/api/players/${id}/voice-mute`, { muted: true }); }
        if (act === "op") await api("POST", `/api/players/${id}/op`);
        if (act === "deop") await api("POST", `/api/players/${id}/deop`);
        if (act === "mode") await api("POST", `/api/players/${id}/gamemode`, { mode: el.value });
        toast("Done"); render();
      } catch (e) { toast(e.message, true); }
    };
    if (act === "mode") el.onchange = handler; else el.onclick = handler;
  });
};

pages.console = async () => {
  main.innerHTML = `
    <h1>Console</h1>
    <div class="console" id="log"></div>
    <div class="console-input">
      <input type="text" id="cmd" placeholder="${hasRole("admin") ? "Type a command and press Enter (no leading slash needed)" : "Read-only: the admin role is required to run commands"}" ${hasRole("admin") ? "" : "disabled"}>
      <button class="btn" id="clear">Clear</button>
    </div>`;
  const log = document.getElementById("log");
  const append = line => {
    const div = document.createElement("div");
    div.className = "line " + line.level;
    div.innerHTML = `<span class="t">${esc(line.time)}</span> <span class="${line.level}">${esc(line.level.padEnd(5))}</span> ${esc(line.message)}`;
    const stick = log.scrollTop + log.clientHeight >= log.scrollHeight - 20;
    log.appendChild(div);
    while (log.children.length > 2000) log.removeChild(log.firstChild);
    if (stick) log.scrollTop = log.scrollHeight;
  };
  if (consoleSocket) consoleSocket.close();
  consoleSocket = new WebSocket((location.protocol === "https:" ? "wss://" : "ws://") + location.host + "/api/console/ws");
  consoleSocket.onmessage = e => append(JSON.parse(e.data));
  consoleSocket.onclose = () => append({ time: "", level: "WARN", message: "console disconnected; reload the page to reconnect" });
  const cmd = document.getElementById("cmd");
  const history = []; let cursor = 0;
  cmd.onkeydown = e => {
    if (e.key === "Enter" && cmd.value.trim()) {
      consoleSocket.send(cmd.value); history.push(cmd.value); cursor = history.length; cmd.value = "";
    } else if (e.key === "ArrowUp" && cursor > 0) { cmd.value = history[--cursor]; }
    else if (e.key === "ArrowDown") { cursor = Math.min(cursor + 1, history.length); cmd.value = history[cursor] || ""; }
  };
  document.getElementById("clear").onclick = () => { log.innerHTML = ""; };
  cmd.focus();
};

pages.mods = async () => {
  const list = await api("GET", "/api/mods");
  main.innerHTML = `
    <h1>Mods <span class="muted">(${list.length})</span></h1>
    ${hasRole("admin") ? `<div class="row" style="margin-bottom:12px"><button class="btn" id="reload">Reload mods from disk</button><span class="help">Drop a mod folder into <code>mods/</code> and reload.</span></div>` : ""}
    <table><thead><tr><th>Mod</th><th>Version</th><th>Type</th><th>Authors</th><th>Status</th><th></th></tr></thead>
    <tbody>${list.map(m => `<tr>
      <td><b>${esc(m.name)}</b><div class="muted">${esc(m.description)}</div><div class="muted mono">${esc(m.id)}</div></td>
      <td>${esc(m.version)}</td><td>${esc(m.kind)}</td><td>${esc(m.authors.join(", "))}</td>
      <td><span class="pill ${m.enabled ? "on" : "off"}">${m.enabled ? "enabled" : "disabled"}</span></td>
      <td class="actions">${hasRole("admin") ? `<button class="btn small" data-id="${esc(m.id)}" data-on="${m.enabled ? 0 : 1}">${m.enabled ? "Disable" : "Enable"}</button>` : ""}</td>
    </tr>`).join("") || `<tr><td colspan="6" class="muted">No mods loaded. Put mods in the <code>mods/</code> folder.</td></tr>`}</tbody></table>`;
  const reload = document.getElementById("reload");
  if (reload) reload.onclick = async () => { await api("POST", "/api/mods/reload"); toast("Mods reloaded"); render(); };
  main.querySelectorAll("button[data-id]").forEach(b => b.onclick = async () => {
    await api("POST", `/api/mods/${encodeURIComponent(b.dataset.id)}/${b.dataset.on === "1" ? "enable" : "disable"}`);
    render();
  });
};

pages.bans = async () => {
  const [bans, whitelist] = await Promise.all([api("GET", "/api/bans"), api("GET", "/api/whitelist")]);
  main.innerHTML = `
    <h1>Bans &amp; whitelist</h1>
    ${hasRole("moderator") ? `<div class="row" style="margin-bottom:12px">
      <input type="text" id="bt" placeholder="Player name, UUID or IP address">
      <input type="text" id="br" placeholder="Reason" style="flex:1">
      <input type="number" id="bh" placeholder="Hours (blank = forever)" style="width:190px">
      <label><input type="checkbox" id="bip"> IP ban</label>
      <button class="btn danger" id="bgo">Ban</button></div>` : ""}
    <table><thead><tr><th>Target</th><th>Reason</th><th>By</th><th>Created</th><th>Expires</th><th></th></tr></thead>
    <tbody>${bans.map(b => `<tr><td><b>${esc(b.name)}</b>${b.uuid ? `<div class="muted mono">${b.uuid}</div>` : ""}</td><td>${esc(b.reason)}</td><td>${esc(b.source)}</td><td class="muted">${esc(b.created)}</td><td class="muted">${esc(b.expires || "never")}</td>
      <td class="actions">${hasRole("moderator") ? `<button class="btn small" data-pardon="${esc(b.name)}">Pardon</button>` : ""}</td></tr>`).join("") || `<tr><td colspan="6" class="muted">No bans.</td></tr>`}</tbody></table>
    <h2>Whitelist</h2>
    ${hasRole("admin") ? `<div class="row" style="margin-bottom:12px"><input type="text" id="wn" placeholder="Player name"><button class="btn" id="wgo">Add</button><span class="help">The whitelist only applies when it is enabled in the config.</span></div>` : ""}
    <div class="row">${whitelist.map(n => `<span class="pill">${esc(n)} ${hasRole("admin") ? `<button class="link" data-wl="${esc(n)}">&times;</button>` : ""}</span>`).join("") || '<span class="muted">Empty.</span>'}</div>`;
  const bgo = document.getElementById("bgo");
  if (bgo) bgo.onclick = async () => {
    try { await api("POST", "/api/bans", { target: bt.value, reason: br.value, hours: bh.value ? Number(bh.value) : null, ip: bip.checked }); toast("Banned"); render(); }
    catch (e) { toast(e.message, true); }
  };
  main.querySelectorAll("[data-pardon]").forEach(b => b.onclick = async () => { await api("DELETE", `/api/bans/${encodeURIComponent(b.dataset.pardon)}`); render(); });
  const wgo = document.getElementById("wgo");
  if (wgo) wgo.onclick = async () => { await api("POST", "/api/whitelist", { name: wn.value }); render(); };
  main.querySelectorAll("[data-wl]").forEach(b => b.onclick = async () => { await api("DELETE", `/api/whitelist/${encodeURIComponent(b.dataset.wl)}`); render(); });
};

pages.anticheat = async () => {
  const list = await api("GET", "/api/violations");
  main.innerHTML = `
    <h1>Anti-cheat</h1>
    <p class="help">Movement, reach and packet checks run server-side. Repeated flags escalate to a kick or ban according to the <code>[anticheat]</code> config section.</p>
    <table><thead><tr><th>Time</th><th>Player</th><th>Check</th><th>Details</th><th>Level</th></tr></thead>
    <tbody>${list.map(v => `<tr><td class="muted">${esc(v.time)}</td><td><b>${esc(v.player)}</b></td><td>${esc(v.check)}</td><td class="mono">${esc(v.details)}</td><td><span class="pill ${v.level >= 3 ? "warn" : ""}">${v.level}</span></td></tr>`).join("") || `<tr><td colspan="5" class="muted">No violations recorded.</td></tr>`}</tbody></table>`;
};

pages.audit = async () => {
  const list = await api("GET", "/api/audit");
  main.innerHTML = `
    <h1>Audit log</h1>
    <table><thead><tr><th>Time</th><th>Who</th><th>Action</th><th>Details</th></tr></thead>
    <tbody>${list.map(a => `<tr><td class="muted">${esc(a.time)}</td><td><b>${esc(a.actor)}</b></td><td>${esc(a.action)}</td><td class="mono">${esc(a.details)}</td></tr>`).join("") || `<tr><td colspan="4" class="muted">Nothing yet.</td></tr>`}</tbody></table>`;
};

pages.config = async () => {
  const text = await api("GET", "/api/config");
  main.innerHTML = `
    <h1>Config</h1>
    <p class="help">This is <code>garnet.toml</code>. Most changes apply after a restart; the file is validated before it is written.${hasRole("owner") ? "" : " Only owners can save changes."}</p>
    <textarea class="code" id="cfg" ${hasRole("owner") ? "" : "readonly"}>${esc(text)}</textarea>
    ${hasRole("owner") ? `<div class="row" style="margin-top:10px"><button class="btn primary" id="save">Save</button></div>` : ""}`;
  const save = document.getElementById("save");
  if (save) save.onclick = async () => {
    try { await api("PUT", "/api/config", document.getElementById("cfg").value); toast("Config saved"); }
    catch (e) { toast(e.message, true); }
  };
};

pages.backups = async () => {
  const list = await api("GET", "/api/backups");
  main.innerHTML = `
    <h1>Backups</h1>
    <div class="row" style="margin-bottom:12px"><button class="btn primary" id="go">Create backup now</button><span class="help">Zips the world folder into <code>backups/</code>. Automatic backups are configured in <code>[backups]</code>.</span></div>
    <table><thead><tr><th>File</th><th>Size</th><th>Created</th></tr></thead>
    <tbody>${list.map(b => `<tr><td class="mono">${esc(b.file)}</td><td>${(b.size_bytes / 1048576).toFixed(1)} MB</td><td class="muted">${esc(b.created)}</td></tr>`).join("") || `<tr><td colspan="3" class="muted">No backups yet.</td></tr>`}</tbody></table>`;
  document.getElementById("go").onclick = async () => { toast("Backing up..."); await api("POST", "/api/backups"); toast("Backup created"); render(); };
};

pages.users = async () => {
  const list = await api("GET", "/api/users");
  main.innerHTML = `
    <h1>Panel users</h1>
    <p class="help">Panel accounts are separate from in-game ops. Roles: <b>viewer</b> (read only), <b>moderator</b> (kick, ban, chat), <b>admin</b> (commands, mods, whitelist, game modes), <b>owner</b> (config, users, stop).</p>
    <div class="row" style="margin-bottom:12px">
      <input type="text" id="nu" placeholder="Username"><input type="password" id="np" placeholder="Password (8+)">
      <select id="nr"><option>viewer</option><option>moderator</option><option>admin</option><option>owner</option></select>
      <button class="btn" id="add">Add user</button></div>
    <table><thead><tr><th>Username</th><th>Role</th><th></th></tr></thead>
    <tbody>${list.map(u => `<tr><td><b>${esc(u.username)}</b></td><td><span class="pill">${esc(u.role)}</span></td>
      <td class="actions"><button class="btn small" data-pw="${esc(u.username)}">Set password</button> <button class="btn small danger" data-del="${esc(u.username)}">Delete</button></td></tr>`).join("")}</tbody></table>`;
  document.getElementById("add").onclick = async () => {
    try { await api("POST", "/api/users", { username: nu.value, password: np.value, role: nr.value }); toast("User added"); render(); }
    catch (e) { toast(e.message, true); }
  };
  main.querySelectorAll("[data-del]").forEach(b => b.onclick = async () => {
    if (!confirm(`Delete ${b.dataset.del}?`)) return;
    try { await api("DELETE", `/api/users/${encodeURIComponent(b.dataset.del)}`); render(); } catch (e) { toast(e.message, true); }
  });
  main.querySelectorAll("[data-pw]").forEach(b => b.onclick = async () => {
    const password = prompt(`New password for ${b.dataset.pw}`); if (!password) return;
    try { await api("POST", `/api/users/${encodeURIComponent(b.dataset.pw)}/password`, { password }); toast("Password changed"); } catch (e) { toast(e.message, true); }
  });
};

// ---------- routing ----------

let refreshTimer = null;

async function render() {
  clearInterval(refreshTimer);
  if (!me) {
    sidebar.hidden = true;
    await pages.login();
    return;
  }
  sidebar.hidden = false;
  document.getElementById("me").textContent = `${me.username} (${me.role})`;
  sidebar.querySelectorAll("nav a").forEach(a => {
    const need = a.dataset.role;
    a.hidden = need && !hasRole(need);
  });
  const page = (location.hash || "#dashboard").slice(1);
  sidebar.querySelectorAll("nav a").forEach(a => a.classList.toggle("active", a.dataset.page === page));
  if (consoleSocket && page !== "console") { consoleSocket.close(); consoleSocket = null; }
  const fn = pages[page] || pages.dashboard;
  try { await fn(); } catch (e) { main.innerHTML = `<div class="error">${esc(e.message)}</div>`; }
  if (page === "dashboard" || page === "players") refreshTimer = setInterval(() => fn().catch(() => {}), 3000);
}

async function boot() {
  try { me = await api("GET", "/api/me"); } catch (_) { me = null; }
  await render();
}

window.addEventListener("hashchange", render);
document.getElementById("logout").onclick = async () => { await api("POST", "/api/logout"); me = null; render(); };
boot();
