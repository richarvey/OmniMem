// Runs before the page paints: a collapsed sidebar goes on <html> first so
// nothing jumps. Kept out of the template so the panel's Content Security
// Policy can forbid inline scripts altogether.
(function () {
  var collapsed;
  try {
    collapsed = localStorage.getItem('omnimem-sidebar-collapsed');
  } catch (e) {}
  if (collapsed === '1') document.documentElement.classList.add('sidebar-collapsed');
})();
