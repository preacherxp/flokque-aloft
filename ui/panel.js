const {
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

function render(view) {
  // Flokque shows only while it runs here.
  const flokque = view.agents !== null;

  if (flokque) {
    byId("sub").replaceChildren("Flokque", pill("Online", "success"));
  } else {
    byId("sub").replaceChildren("Keeps working with the lid closed");
  }

  byId("bird").classList.toggle("on", view.on);
  byId("bird").querySelector(".glyph").src = view.on ? "bird-on.png" : "bird-off.png";
  byId("keep").checked = view.on;
  byId("detail").textContent = view.detail;
  byId("detail").classList.toggle("problem", view.problem);

  byId("agents-line").hidden = !flokque;
  byId("agents").replaceChildren(pill(`${view.agents} running`, view.agents > 0 ? "primary" : "muted"));
  byId("lid-line").hidden = view.lid === null;
  byId("lid").textContent = view.lid ?? "";
  byId("power").textContent = view.power;
  byId("open").hidden = !view.canOpenFlokque;

  getCurrentWindow().setSize(new LogicalSize(290, document.querySelector(".panel").offsetHeight));
}

byId("keep").addEventListener("change", (event) => {
  // Shows the switch as it was until Aloft says whether it changed.
  const on = event.target.checked;
  event.target.checked = !on;
  invoke("set_on", { on });
});

byId("open").addEventListener("click", () => invoke("open_flokque"));

byId("quit").addEventListener("click", () => invoke("quit"));

document.addEventListener("keydown", (event) => {
  if (event.key === "Escape") invoke("hide");
});

listen("state", (event) => render(event.payload));

invoke("state").then(render);
