// In-page replacement for the native <select> popup. Keeping the menu in the
// page preserves CEF view coordinates and lets it inherit the hosted app's
// visual language instead of looking like a separate runtime surface.
(function () {
    var open = null;
    var edge = 4;

    function isDropdown(el) {
        return el && el.tagName === 'SELECT' && !el.multiple && el.size <= 1 && !el.disabled;
    }

    function closeOpen() {
        if (open) open();
    }

    function openMenu(select) {
        closeOpen();

        var computed = getComputedStyle(select);
        var background = computed.backgroundColor;
        var foreground = computed.color;
        var border = computed.borderTopColor;
        var radius = computed.borderRadius;
        var accent = computed.accentColor;
        if (!accent || accent === 'auto') accent = 'rgb(79, 70, 229)';

        var host = document.createElement('div');
        host.id = '_jselect';
        host.style.cssText = 'position:fixed;inset:0;z-index:2147483647';
        var shadow = host.attachShadow({mode: 'closed'});

        var style = document.createElement('style');
        style.textContent =
            '*{margin:0;padding:0;box-sizing:border-box;user-select:none}' +
            '.bg{position:fixed;inset:0}' +
            '.m{position:fixed;background:var(--background);border:1px solid var(--border);' +
              'border-radius:var(--radius);padding:4px 0;overflow-y:auto;overscroll-behavior:contain;' +
              'font-family:var(--font-family);font-size:var(--font-size);font-weight:var(--font-weight);' +
              'line-height:var(--line-height);color:var(--foreground);' +
              'box-shadow:0 10px 25px rgba(0,0,0,.45);outline:none}' +
            '.i{padding:5px 24px 5px 12px;cursor:default;white-space:nowrap}' +
            '.i:hover,.i.a{background:var(--accent);color:white}' +
            '.i.sel{font-weight:600}' +
            '.i.off{opacity:.45;pointer-events:none}' +
            '.g{padding:5px 12px 2px;opacity:.7;font-weight:600;cursor:default;white-space:nowrap}';
        shadow.appendChild(style);

        var bg = document.createElement('div');
        bg.className = 'bg';
        shadow.appendChild(bg);

        var menu = document.createElement('div');
        menu.className = 'm';
        menu.setAttribute('role', 'listbox');
        menu.style.setProperty('--background', background);
        menu.style.setProperty('--foreground', foreground);
        menu.style.setProperty('--border', border);
        menu.style.setProperty('--radius', radius);
        menu.style.setProperty('--accent', accent);
        menu.style.setProperty('--font-family', computed.fontFamily);
        menu.style.setProperty('--font-size', computed.fontSize);
        menu.style.setProperty('--font-weight', computed.fontWeight);
        menu.style.setProperty('--line-height', computed.lineHeight);
        shadow.appendChild(menu);

        // Key rows by opt.index, not row position, so disabled options and
        // optgroup headers don't skew the map back to selectedIndex.
        var rows = [];
        var rowIndex = [];
        function addOption(opt) {
            var el = document.createElement('div');
            var off = opt.disabled || (opt.parentNode && opt.parentNode.tagName === 'OPTGROUP' && opt.parentNode.disabled);
            el.className = 'i' + (off ? ' off' : '') + (opt.index === select.selectedIndex ? ' sel' : '');
            el.textContent = opt.text;
            el.setAttribute('role', 'option');
            el.setAttribute('aria-selected', opt.index === select.selectedIndex ? 'true' : 'false');
            if (off) el.setAttribute('aria-disabled', 'true');
            menu.appendChild(el);
            if (!off) {
                el.dataset.idx = opt.index;
                rows.push(el);
                rowIndex.push(opt.index);
            }
        }
        for (var i = 0; i < select.children.length; i++) {
            var child = select.children[i];
            if (child.tagName === 'OPTGROUP') {
                var g = document.createElement('div');
                g.className = 'g';
                g.textContent = child.label;
                menu.appendChild(g);
                for (var j = 0; j < child.children.length; j++) {
                    if (child.children[j].tagName === 'OPTION') addOption(child.children[j]);
                }
            } else if (child.tagName === 'OPTION') {
                addOption(child);
            }
        }

        var r = select.getBoundingClientRect();
        menu.style.minWidth = Math.min(r.width, innerWidth - edge * 2) + 'px';

        var active = -1;
        function setActive(n) {
            if (active >= 0) rows[active].classList.remove('a');
            active = n;
            if (active >= 0) {
                rows[active].classList.add('a');
                menu.setAttribute('aria-activedescendant', rows[active].id);
                rows[active].scrollIntoView({block: 'nearest'});
            } else {
                menu.removeAttribute('aria-activedescendant');
            }
        }

        var done = false;
        function finish(idx) {
            if (done) return;
            done = true;
            open = null;
            window.removeEventListener('keydown', onKeyDown, true);
            window.removeEventListener('blur', onDismiss);
            window.removeEventListener('resize', onDismiss);
            document.removeEventListener('scroll', onDismiss, true);
            host.remove();
            if (idx != null && idx !== select.selectedIndex) {
                select.selectedIndex = idx;
                select.dispatchEvent(new Event('input', {bubbles: true}));
                select.dispatchEvent(new Event('change', {bubbles: true}));
            }
        }
        open = function () { finish(null); };
        function onDismiss() { finish(null); }

        function onKeyDown(e) {
            if (e.key === 'Escape') { e.preventDefault(); finish(null); }
            else if (e.key === 'ArrowDown') { e.preventDefault(); setActive(active < rows.length - 1 ? active + 1 : 0); }
            else if (e.key === 'ArrowUp') { e.preventDefault(); setActive(active > 0 ? active - 1 : rows.length - 1); }
            else if ((e.key === 'Enter' || e.key === ' ') && active >= 0) { e.preventDefault(); finish(rowIndex[active]); }
            else if (e.key === 'Tab') { finish(null); }
        }

        menu.addEventListener('mousedown', function (e) {
            e.preventDefault();
            var t = e.target.closest('.i:not(.off)');
            if (t) finish(parseInt(t.dataset.idx));
        });
        bg.addEventListener('mousedown', function (e) {
            e.preventDefault();
            finish(null);
        });
        window.addEventListener('keydown', onKeyDown, true);
        window.addEventListener('blur', onDismiss);
        window.addEventListener('resize', onDismiss);
        document.addEventListener('scroll', onDismiss, true);

        document.body.appendChild(host);

        // Measure in the same CEF view coordinate space as the select. Prefer
        // opening below, but use the larger side when the menu would not fit.
        var naturalHeight = menu.scrollHeight;
        var below = Math.max(0, innerHeight - r.bottom - edge);
        var above = Math.max(0, r.top - edge);
        var openBelow = below >= Math.min(naturalHeight, 240) || below >= above;
        var available = openBelow ? below : above;
        menu.style.maxHeight = available + 'px';
        menu.style.top = (openBelow ? r.bottom : Math.max(edge, r.top - Math.min(naturalHeight, available))) + 'px';

        var menuWidth = menu.getBoundingClientRect().width;
        menu.style.left = Math.min(
            Math.max(edge, r.left),
            Math.max(edge, innerWidth - menuWidth - edge)
        ) + 'px';

        for (var k = 0; k < rowIndex.length; k++) {
            rows[k].id = '_jselect-option-' + rowIndex[k];
            if (rowIndex[k] === select.selectedIndex) { setActive(k); break; }
        }
    }

    // Capture phase so we intercept before the engine opens the native popup.
    document.addEventListener('mousedown', function (e) {
        if (e.button !== 0) return;
        var select = e.target.closest && e.target.closest('select');
        if (!isDropdown(select)) return;
        e.preventDefault();
        if (open) { closeOpen(); return; }
        select.focus();
        openMenu(select);
    }, true);

    document.addEventListener('keydown', function (e) {
        // While open, the menu's own capture-phase handler owns the keyboard.
        if (open) return;
        if (!isDropdown(document.activeElement)) return;
        var opens = e.key === ' ' || e.key === 'Enter' || e.key === 'F4' ||
            (e.altKey && (e.key === 'ArrowDown' || e.key === 'ArrowUp'));
        if (!opens) return;
        e.preventDefault();
        openMenu(document.activeElement);
    }, true);
})();
