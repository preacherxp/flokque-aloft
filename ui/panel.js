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
  const sub = byId("sub");

  if (view.flokque) {
    sub.replaceChildren(view.server, view.flokque === "online" ? pill("Online", "success") : pill("Offline", "muted"));
  } else {
    sub.replaceChildren("Keeps working with the lid closed");
  }

  byId("keep").checked = view.on;
  byId("detail").textContent = view.detail;
  byId("detail").classList.toggle("problem", view.problem);

  byId("agents-line").hidden = !view.flokque;
  byId("agents").replaceChildren(
    view.agents === null ? "—" : pill(`${view.agents} running`, view.agents > 0 ? "primary" : "muted"),
  );
  byId("lid-line").hidden = view.lid === null;
  byId("lid").textContent = view.lid ?? "";
  byId("power").textContent = view.power;

  getCurrentWindow().setSize(new LogicalSize(290, document.querySelector(".panel").offsetHeight));
}

byId("keep").addEventListener("change", (event) => {
  // Shows the switch as it was until Aloft says whether it changed.
  const on = event.target.checked;
  event.target.checked = !on;
  invoke("set_on", { on });
});

byId("quit").addEventListener("click", () => invoke("quit"));

document.addEventListener("keydown", (event) => {
  if (event.key === "Escape") invoke("hide");
});

listen("state", (event) => render(event.payload));

invoke("state").then(render);
