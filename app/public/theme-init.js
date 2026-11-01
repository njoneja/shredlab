// Runs before first paint (a plain blocking script in <head>, so the CSP's
// script-src 'self' allows it): applies the theme mirrored in localStorage,
// so there is no flash of the wrong theme while the app settings load.
// The backend's app settings stay the source of truth (lib/theme.ts).
(function () {
  var themes = ["stage", "rosewood", "valve", "manuscript"];
  var t = null;
  try {
    t = localStorage.getItem("shredlab.theme");
  } catch (e) {
    /* storage unavailable */
  }
  document.documentElement.setAttribute("data-theme", themes.indexOf(t) >= 0 ? t : "stage");
})();
