// The panel's behaviour, driven by data attributes. Templates carry no inline
// JavaScript or event handlers: memory content, feed names and skill names
// are rendered by the templates, and a page that puts such text inside a
// script is a page that can be made to run it. The Content Security Policy
// in base.html forbids inline scripts, so everything lives here.
(function () {
  var root = document.documentElement;

  function byId(id) {
    return id ? document.getElementById(id) : null;
  }

  // Sidebar collapse, remembered per browser.
  (function () {
    var KEY = 'omnimem-sidebar-collapsed';
    var toggles = document.querySelectorAll('[data-sidebar-toggle]');
    function apply(collapsed) {
      root.classList.toggle('sidebar-collapsed', collapsed);
      toggles.forEach(function (btn) { btn.setAttribute('aria-expanded', String(!collapsed)); });
    }
    toggles.forEach(function (btn) {
      btn.addEventListener('click', function () {
        var collapsed = !root.classList.contains('sidebar-collapsed');
        try { localStorage.setItem(KEY, collapsed ? '1' : '0'); } catch (e) {}
        apply(collapsed);
      });
    });
    apply(root.classList.contains('sidebar-collapsed'));
  })();

  // Light and dark theme, remembered per browser.
  (function () {
    var KEY = 'omnimem-theme';
    var btn = document.querySelector('[data-theme-toggle]');
    if (!btn) return;
    function label() {
      var next = root.getAttribute('data-theme') === 'dark' ? 'light' : 'dark';
      btn.textContent = next === 'light' ? 'Light theme' : 'Dark theme';
      btn.setAttribute('aria-label', 'Switch to ' + next + ' theme');
    }
    btn.addEventListener('click', function () {
      var next = root.getAttribute('data-theme') === 'dark' ? 'light' : 'dark';
      root.setAttribute('data-theme', next);
      try { localStorage.setItem(KEY, next); } catch (e) {}
      label();
    });
    label();
  })();

  // Clicks: dismiss a flash, open or close a modal, proxy a click to a
  // hidden file input, add a skill row, or fill the domains field from a
  // suggestion. Delegated, so rows htmx swaps in later work too.
  document.addEventListener('click', function (event) {
    var el = event.target.closest('[data-dismiss], [data-show], [data-hide], [data-click-target], [data-add-skill-row], [data-fill-domains]');
    if (!el) return;
    if (el.hasAttribute('data-dismiss')) {
      el.parentElement.remove();
      return;
    }
    if (el.hasAttribute('data-click-target')) {
      var target = byId(el.getAttribute('data-click-target'));
      if (target) target.click();
      return;
    }
    if (el.hasAttribute('data-show')) {
      var shown = byId(el.getAttribute('data-show'));
      if (shown) shown.style.display = 'flex';
      return;
    }
    if (el.hasAttribute('data-hide')) {
      var hidden = byId(el.getAttribute('data-hide'));
      if (hidden) hidden.style.display = 'none';
      var cleared = byId(el.getAttribute('data-clear'));
      if (cleared) cleared.innerHTML = '';
      return;
    }
    if (el.hasAttribute('data-add-skill-row')) {
      var rows = byId('skill-rows');
      if (!rows || !rows.lastElementChild) return;
      var row = rows.lastElementChild.cloneNode(true);
      var domain = row.querySelector('input[name="skill_domain"]');
      var influence = row.querySelector('input[name="skill_influence"]');
      if (domain) domain.value = '';
      if (influence) influence.value = '5';
      rows.appendChild(row);
      return;
    }
    if (el.hasAttribute('data-fill-domains')) {
      var field = byId(el.getAttribute('data-fill-domains'));
      if (field) field.value = el.getAttribute('data-domains') || '';
      var suggestion = el.closest('.domain-suggestion');
      if (suggestion) suggestion.remove();
    }
  });

  // A file input that submits its form as soon as a file is chosen.
  document.addEventListener('change', function (event) {
    var input = event.target.closest('[data-submit-on-change]');
    if (input && input.form) input.form.submit();
  });

  // Destructive forms ask first. The question is plain text in an
  // attribute, so a name inside it is only ever text.
  document.addEventListener('submit', function (event) {
    var form = event.target.closest('[data-confirm]');
    if (form && !window.confirm(form.getAttribute('data-confirm'))) {
      event.preventDefault();
    }
  });

  // The starting page polls until the services are up.
  var reload = document.querySelector('[data-reload-after]');
  if (reload) {
    var delay = parseInt(reload.getAttribute('data-reload-after'), 10) || 2000;
    setTimeout(function () { location.reload(); }, delay);
  }
})();
