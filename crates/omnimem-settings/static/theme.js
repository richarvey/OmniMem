// Runs before the page paints: the saved sidebar and theme preferences go on
// <html> first so nothing flashes. Kept out of the template so the panel's
// Content Security Policy can forbid inline scripts altogether.
(function () {
  var root = document.documentElement;
  var theme, collapsed;
  try {
    collapsed = localStorage.getItem('omnimem-sidebar-collapsed');
    theme = localStorage.getItem('omnimem-theme');
  } catch (e) {}
  if (collapsed === '1') root.classList.add('sidebar-collapsed');
  if (theme !== 'light' && theme !== 'dark') {
    theme = window.matchMedia && window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
  }
  root.setAttribute('data-theme', theme);
})();
