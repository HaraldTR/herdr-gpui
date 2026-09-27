// Herdr GPUI's annotation picker. The app evaluates this in the page's main
// frame when annotating starts, passing the numbered notes already queued.
// It only reports what the user picks; the notes themselves are written in
// the app, where the page cannot read them. Everything it posts is treated
// as untrusted by the app, and nothing reaches an agent until the user sends.
(function (markers) {
  "use strict";
  var previous = window.__herdrAnnotate;
  if (previous && typeof previous.disarm === "function") {
    try { previous.disarm(); } catch (_) {}
  }
  var post = function (message) {
    try { window.ipc.postMessage(JSON.stringify(message)); } catch (_) {}
  };

  var host = document.createElement("div");
  host.style.cssText =
    "position:fixed;inset:0;z-index:2147483647;pointer-events:none;margin:0;padding:0;border:0";
  var root = host.attachShadow({ mode: "closed" });
  root.innerHTML =
    "<style>" +
    ".box{position:fixed;border:2px solid #0d99ff;background:rgba(13,153,255,.08);border-radius:2px;display:none}" +
    ".label{position:fixed;background:#0d99ff;color:#fff;font:11px/16px -apple-system,system-ui,sans-serif;padding:0 6px;border-radius:3px;display:none;white-space:nowrap}" +
    ".mark{position:fixed;border:2px dashed #f59e0b;border-radius:2px}" +
    ".pin{position:fixed;min-width:18px;height:18px;padding:0 4px;box-sizing:border-box;border-radius:9px;background:#f59e0b;color:#111;font:bold 11px/18px -apple-system,system-ui,sans-serif;text-align:center;box-shadow:0 1px 3px rgba(0,0,0,.4)}" +
    "</style><div class=box></div><div class=label></div><div class=marks></div>";
  var box = root.querySelector(".box");
  var label = root.querySelector(".label");
  var marks = root.querySelector(".marks");
  document.documentElement.appendChild(host);

  var ownsNode = function (node) {
    return node === host || host.contains(node);
  };

  // A path of tag:nth-of-type steps from <body>, or from the nearest
  // element with a unique id.
  var selectorOf = function (element) {
    var steps = [];
    for (var node = element; node && node.nodeType === 1 && node !== document.documentElement; node = node.parentElement) {
      if (node.id && window.CSS && CSS.escape) {
        var byId = "#" + CSS.escape(node.id);
        try {
          if (document.querySelectorAll(byId).length === 1) {
            steps.unshift(byId);
            break;
          }
        } catch (_) {}
      }
      var tag = node.tagName.toLowerCase();
      if (node === document.body) {
        steps.unshift("body");
        break;
      }
      var index = 1;
      for (var sibling = node.previousElementSibling; sibling; sibling = sibling.previousElementSibling) {
        if (sibling.tagName === node.tagName) index += 1;
      }
      steps.unshift(tag + ":nth-of-type(" + index + ")");
    }
    return steps.join(" > ");
  };

  var textOf = function (element) {
    return String(element.innerText || element.textContent || "").replace(/\s+/g, " ").trim().slice(0, 400);
  };

  var elementAt = function (x, y) {
    var element = document.elementFromPoint(x, y);
    return element && !ownsNode(element) ? element : null;
  };

  var place = function (node, left, top, width, height) {
    node.style.left = left + "px";
    node.style.top = top + "px";
    if (width !== undefined) node.style.width = width + "px";
    if (height !== undefined) node.style.height = height + "px";
  };

  var hovered = null;
  var showBox = function (element) {
    hovered = element;
    if (!element) {
      box.style.display = "none";
      label.style.display = "none";
      return;
    }
    var rect = element.getBoundingClientRect();
    place(box, rect.left, rect.top, rect.width, rect.height);
    box.style.display = "block";
    label.textContent = element.tagName.toLowerCase() + "  " + Math.round(rect.width) + "×" + Math.round(rect.height);
    place(label, Math.max(0, rect.left), rect.top >= 20 ? rect.top - 18 : rect.bottom + 2);
    label.style.display = "block";
  };

  var drawMarkers = function () {
    marks.textContent = "";
    markers.forEach(function (marker) {
      var element = null;
      try { element = marker.selector ? document.querySelector(marker.selector) : null; } catch (_) {}
      if (!element) return;
      var rect = element.getBoundingClientRect();
      var mark = document.createElement("div");
      mark.className = "mark";
      place(mark, rect.left, rect.top, rect.width, rect.height);
      var pin = document.createElement("div");
      pin.className = "pin";
      pin.textContent = String(marker.number);
      place(pin, Math.max(0, rect.left - 9), Math.max(0, rect.top - 9));
      marks.appendChild(mark);
      marks.appendChild(pin);
    });
  };
  var frame = 0;
  var redraw = function () {
    if (frame) return;
    frame = requestAnimationFrame(function () {
      frame = 0;
      drawMarkers();
      if (hovered) showBox(hovered);
    });
  };

  var swallow = function (event) {
    event.preventDefault();
    event.stopPropagation();
    event.stopImmediatePropagation();
  };
  var onMove = function (event) {
    showBox(elementAt(event.clientX, event.clientY));
  };
  // Text selection needs the default press, so presses only stop reaching
  // the page's own handlers; clicks are swallowed so links do not follow.
  var onPress = function (event) {
    event.stopPropagation();
    event.stopImmediatePropagation();
  };
  var onRelease = function (event) {
    if (event.button !== 0) return;
    event.stopPropagation();
    event.stopImmediatePropagation();
    var selection = window.getSelection();
    var quote = selection && !selection.isCollapsed ? String(selection).replace(/\s+/g, " ").trim() : "";
    if (quote) {
      var node = selection.getRangeAt(0).commonAncestorContainer;
      var element = node.nodeType === 1 ? node : node.parentElement;
      if (element && !ownsNode(element)) {
        post({ kind: "pick", target: { kind: "selection", selector: selectorOf(element), tag: element.tagName.toLowerCase(), quote: quote.slice(0, 1000) } });
      }
      return;
    }
    var picked = elementAt(event.clientX, event.clientY);
    if (!picked) return;
    post({
      kind: "pick",
      target: {
        kind: "element",
        selector: selectorOf(picked),
        tag: picked.tagName.toLowerCase(),
        text: textOf(picked),
        html: String(picked.outerHTML || "").slice(0, 2000),
      },
    });
  };
  var onKey = function (event) {
    if (event.key === "Escape") {
      swallow(event);
      post({ kind: "cancel" });
    }
  };

  var listeners = [
    ["mousemove", onMove],
    ["mousedown", onPress],
    ["pointerdown", onPress],
    ["mouseup", onRelease],
    ["click", swallow],
    ["dblclick", swallow],
    ["auxclick", swallow],
    ["submit", swallow],
    ["keydown", onKey],
  ];
  listeners.forEach(function (entry) { window.addEventListener(entry[0], entry[1], true); });
  window.addEventListener("scroll", redraw, true);
  window.addEventListener("resize", redraw, true);
  drawMarkers();

  window.__herdrAnnotate = {
    disarm: function () {
      listeners.forEach(function (entry) { window.removeEventListener(entry[0], entry[1], true); });
      window.removeEventListener("scroll", redraw, true);
      window.removeEventListener("resize", redraw, true);
      if (frame) cancelAnimationFrame(frame);
      host.remove();
      if (window.__herdrAnnotate && window.__herdrAnnotate.host === host) delete window.__herdrAnnotate;
    },
    host: host,
  };
})
