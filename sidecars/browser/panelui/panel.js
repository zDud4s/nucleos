// Placeholder panel bundle, overwritten by the panel build. It mounts <nucleos-panel> in a closed
// shadow root and defines the one entry the driver pushes messages through.
(() => {
  if (globalThis.__nucleosPush) return;
  class NucleosPanel extends HTMLElement {
    constructor() {
      super();
      this.attachShadow({ mode: 'closed' });
    }
  }
  if (!customElements.get('nucleos-panel')) customElements.define('nucleos-panel', NucleosPanel);
  const mount = () => {
    if (!document.documentElement) return;
    document.documentElement.appendChild(document.createElement('nucleos-panel'));
  };
  if (document.documentElement) mount();
  else document.addEventListener('DOMContentLoaded', mount, { once: true });
  globalThis.__nucleosPush = () => {};
})();
