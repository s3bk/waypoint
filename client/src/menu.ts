// The burger menu: every tool lives behind it, and at most one tool is open at a time.
// Opening a tool closes the others by clicking their own button, which runs the tool's own close code.

interface Tool {
  button: HTMLElement;
  panel: HTMLElement;
}

export function setupMenu(tools: Tool[]) {
  const closeAll = () => {
    closeTools();
    closeMenu();
    sync();
  };
  const burger = document.getElementById("menu-button")!;
  const menu = document.getElementById("menu")!;
  const isOpen = (t: Tool) => t.panel.style.display === "block";
  let closing = false;

  const sync = () => {
    const any = tools.some(isOpen);
    burger.classList.toggle("on", any || menu.classList.contains("open"));
    burger.setAttribute("aria-expanded", String(menu.classList.contains("open")));
  };
  const closeTools = (except?: Tool) => {
    closing = true;
    for (const t of tools) if (t !== except && isOpen(t)) t.button.click();
    closing = false;
  };
  const closeMenu = () => menu.classList.remove("open");

  // Capture phase: runs before the tool's own click handler, so the others are closed first.
  menu.addEventListener(
    "click",
    (e) => {
      if (closing) return;
      const hit = (e.target as Element).closest("button");
      const tool = tools.find((t) => t.button === hit);
      if (tool) closeTools(tool);
      // The menu folds away once something was picked; a tool's panel stays.
      closeMenu();
      queueMicrotask(sync);
    },
    true,
  );
  burger.addEventListener("click", () => {
    // With a tool open the burger closes it and shows the menu, to pick another; otherwise it toggles the menu.
    if (tools.some(isOpen)) {
      closeTools();
      menu.classList.add("open");
    } else {
      menu.classList.toggle("open");
    }
    sync();
  });
  // A press elsewhere closes the menu and any open tool (the types panel sits outside #share).
  document.addEventListener("pointerdown", (e) => {
    if ((e.target as Element).closest("#share, #types-panel")) return;
    if (menu.classList.contains("open") || tools.some(isOpen)) closeAll();
  });
  return { closeAll };
}
