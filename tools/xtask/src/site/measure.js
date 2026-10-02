(function (sels) {
  // checkVisibility also sees content hidden inside a closed <details>.
  function vis(el) { return el.checkVisibility ? el.checkVisibility() : el.getClientRects().length > 0; }
  function find(sel) {
    if (sel.indexOf("text=") === 0) {
      var want = sel.slice(5).trim();
      var els = document.querySelectorAll("h1,h2,h3,h4,a,p,span,summary,li,td,dt,button");
      for (var i = 0; i < els.length; i++) {
        var t = (els[i].textContent || "").replace(/\s+/g, " ").trim();
        if (t.indexOf(want) === 0 && vis(els[i])) return els[i];
      }
      return null;
    }
    if (/^#[^ .\[]+$/.test(sel)) {
      var byId = document.getElementById(sel.slice(1));
      return byId && vis(byId) ? byId : null;
    }
    var all;
    try { all = document.querySelectorAll(sel); } catch (e) { return null; }
    for (var j = 0; j < all.length; j++) if (vis(all[j])) return all[j];
    return null;
  }
  var out = {vh: innerHeight, vw: innerWidth, header: 0, els: {}};
  var hdr = document.querySelector(".site-header");
  if (hdr) out.header = hdr.getBoundingClientRect().height;
  sels.forEach(function (sel) {
    var el = find(sel);
    if (!el) { out.els[sel] = null; return; }
    var r = el.getBoundingClientRect();
    out.els[sel] = {top: r.top + scrollY, sticky: !!el.closest(".site-header")};
  });
  return out;
})
