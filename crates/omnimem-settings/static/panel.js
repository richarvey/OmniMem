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

  // The menu is a <details>, open in the markup so it works without
  // scripts. On a phone it folds behind its Menu toggle until asked for.
  (function () {
    var menu = document.querySelector('[data-nav-menu]');
    if (!menu || !window.matchMedia) return;
    var phone = window.matchMedia('(max-width: 768px)');
    function fit() { menu.open = !phone.matches; }
    fit();
    if (phone.addEventListener) phone.addEventListener('change', fit);
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

  // A file input that submits its form as soon as a file is chosen, and a
  // filter form whose selects apply as soon as they change.
  document.addEventListener('change', function (event) {
    var input = event.target.closest('[data-submit-on-change]');
    if (input && input.form) { input.form.submit(); return; }
    var select = event.target.closest('select');
    var auto = select && select.form && select.form.hasAttribute('data-auto-submit');
    if (auto) select.form.submit();
  });

  // Destructive forms ask first. The question is plain text in an
  // attribute, so a name inside it is only ever text.
  document.addEventListener('submit', function (event) {
    var form = event.target.closest('[data-confirm]');
    if (form && !window.confirm(form.getAttribute('data-confirm'))) {
      event.preventDefault();
      return;
    }
    // A slow GET (a duplicate scan) says it is working and can't be sent twice.
    var busy = event.target.closest('[data-busy-on-submit]');
    if (busy) {
      var note = busy.querySelector('.busy-note');
      if (note) note.textContent = 'Scanning…';
      busy.querySelectorAll('button[type="submit"]').forEach(function (b) { b.disabled = true; });
    }
  });

  // A duplicate cluster left with one member after an in-place archive is
  // settled: say so instead of showing a cluster of one.
  document.addEventListener('htmx:afterSwap', function (event) {
    var cluster = event.target.closest ? event.target.closest('.cluster') : null;
    document.querySelectorAll('.cluster').forEach(function (c) {
      if (c.querySelectorAll('.cluster-row').length < 2) {
        c.classList.add('cluster-settled');
        c.innerHTML = '<p class="cluster-head">Settled: one copy left.</p>';
      }
    });
    if (cluster) cluster = null;
  });

  // The starting page polls until the services are up.
  var reload = document.querySelector('[data-reload-after]');
  if (reload) {
    var delay = parseInt(reload.getAttribute('data-reload-after'), 10) || 2000;
    setTimeout(function () { location.reload(); }, delay);
  }
})();
