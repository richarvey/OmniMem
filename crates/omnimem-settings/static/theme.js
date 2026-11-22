// Runs before the page paints, so nothing flashes or jumps: a collapsed
// sidebar and the colour theme go on <html> first. Kept out of the template
// so the panel's Content Security Policy can forbid inline scripts
// altogether.
(function () {
  var root = document.documentElement;
  var collapsed;
  try {
    collapsed = localStorage.getItem('omnimem-sidebar-collapsed');
  } catch (e) {}
  if (collapsed === '1') root.classList.add('sidebar-collapsed');

  // The theme is the one the person picked in this browser, or failing
  // that whatever the system prefers. panel.js drives the switch.
  var KEY = 'omnimem-theme';
  var system = window.matchMedia ? window.matchMedia('(prefers-color-scheme: light)') : null;
  function stored() {
    var value = null;
    try { value = localStorage.getItem(KEY); } catch (e) {}
    return value === 'light' || value === 'dark' ? value : null;
  }
  function apply(theme) {
    root.setAttribute('data-theme', theme);
    var meta = document.querySelector('meta[name="theme-color"]');
    if (meta) meta.setAttribute('content', theme === 'light' ? '#f5f6f8' : '#0b0c0e');
  }
  function preferred() {
    return stored() || (system && system.matches ? 'light' : 'dark');
  }
  apply(preferred());

  window.omnimemTheme = {
    current: function () { return root.getAttribute('data-theme'); },
    set: function (theme) {
      try { localStorage.setItem(KEY, theme); } catch (e) {}
      apply(theme);
    },
    // Follow the system while nothing has been picked here.
    watch: function (onChange) {
      if (!system || !system.addEventListener) return;
      system.addEventListener('change', function () {
        if (stored()) return;
        apply(preferred());
        onChange();
      });
    }
  };
})();
