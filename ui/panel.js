const {
  app: { getVersion },
  core: { invoke },
  event: { listen },
  window: { getCurrentWindow, LogicalSize },
} = window.__TAURI__;

const byId = (id) => document.getElementById(id);

function pill(text, tone) {
  const element = document.createElement("span");

  element.className = "pill";
  element.textContent = text;
  element.style.setProperty("--tone", `var(--${tone})`);

  return element;
}

/** The window is as tall as the page it shows. */
function fit() {
  getCurrentWindow().setSize(new LogicalSize(290, document.querySelector(".panel").offsetHeight));
}

function show(page) {
  byId("home").hidden = page !== "home";
  byId("about").hidden = page !== "about";
  fit();
}

function render(view) {
  // Flokque shows only while it runs here.
  const flokque = view.agents !== null;

  if (flokque) {
    byId("sub").replaceChildren("Flokque", pill("Online", "success"));
  } else {
    byId("sub").replaceChildren("Keeps working with the lid closed");
  }

  byId("icon").src = view.on ? "icon-on.png" : "icon-off.png";
  byId("keep").checked = view.on;
  byId("detail").textContent = view.detail;
  byId("detail").classList.toggle("problem", view.problem);

  byId("agents-line").hidden = !flokque;
  byId("agents").replaceChildren(pill(`${view.agents} running`, view.agents > 0 ? "primary" : "muted"));
  byId("lid-line").hidden = view.lid === null;
  byId("lid").textContent = view.lid ?? "";
  byId("power").textContent = view.power;
  byId("open").hidden = !view.canOpenFlokque;

  fit();
}

byId("keep").addEventListener("change", (event) => {
  // Shows the switch as it was until Aloft says whether it changed.
  const on = event.target.checked;
  event.target.checked = !on;
  invoke("set_on", { on });
});

byId("open").addEventListener("click", () => invoke("open_flokque"));

byId("show-about").addEventListener("click", () => show("about"));

byId("quit").addEventListener("click", () => invoke("quit"));

byId("source").addEventListener("click", () => invoke("open_link", { link: "source" }));

byId("coffee").addEventListener("click", () => invoke("open_link", { link: "coffee" }));

byId("back").addEventListener("click", () => show("home"));

document.addEventListener("keydown", (event) => {
  if (event.key !== "Escape") return;

  if (byId("about").hidden) invoke("hide");
  else show("home");
});

// The panel hides when it loses focus, and opens on its main page next time.
window.addEventListener("blur", () => show("home"));

listen("state", (event) => render(event.payload));

listen("page", (event) => show(event.payload));

invoke("state").then(render);

getVersion().then((version) => {
  byId("version").textContent = `Version ${version}`;
});
