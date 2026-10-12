/**
 * Silicon DM's logo, as the Carbon drew it. `LOGO` is the whole icon, the mark in #181818 on a cream (#EDE8E0) square,
 * as a 200 by 200 SVG document: the browser and home-screen icons, the Open Graph image and the Silicon Apps listing use
 * it (lib/brand-svg.ts). `LOGO_MARK` is the mark alone, drawn in the text colour (currentColor), as the header shows it
 * beside the name (components/foundation/brand/app-mark.tsx).
 */
export const LOGO = `<svg width="200" height="200" viewBox="0 0 200 200" fill="none" xmlns="http://www.w3.org/2000/svg"><rect width="200" height="200" fill="#EDE8E0"/><rect x="30.7875" y="109.437" width="117.975" height="60.7748" fill="#181818" stroke="#EDE8E0" stroke-width="3.57499"/><rect x="52.2377" y="30.7875" width="117.975" height="60.7748" fill="#181818" stroke="#EDE8E0" stroke-width="3.57499"/></svg>`;

export const LOGO_MARK = {
  viewBox: "28 28 144 144",
  body: `<rect x="30.7875" y="109.437" width="117.975" height="60.7748" fill="currentColor" style="stroke:var(--background, #FFFDF9)" stroke-width="3.57499"/><rect x="52.2377" y="30.7875" width="117.975" height="60.7748" fill="currentColor" style="stroke:var(--background, #FFFDF9)" stroke-width="3.57499"/>`,
};
