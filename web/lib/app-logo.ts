/**
 * Silicon Commit's logo, as the Carbon drew it. `LOGO` is the whole icon, the mark in #181818 on a cream (#EDE8E0) square,
 * as a 200 by 200 SVG document: the browser and home-screen icons, the Open Graph image and the Silicon Apps listing use
 * it (lib/brand-svg.ts). `LOGO_MARK` is the mark alone, drawn in the text colour (currentColor), as the header shows it
 * beside the name (components/foundation/brand/app-mark.tsx).
 */
export const LOGO = `<svg width="200" height="200" viewBox="0 0 200 200" fill="none" xmlns="http://www.w3.org/2000/svg"><rect width="200" height="200" fill="#EDE8E0"/><circle cx="99.5" cy="99.5" r="28.7625" stroke="#181818" stroke-width="1.475" stroke-dasharray="3 3"/><path d="M99.5 70C95.626 70 91.7899 70.763 88.2108 72.2456C84.6317 73.7281 81.3797 75.901 78.6403 78.6404C75.901 81.3797 73.7281 84.6317 72.2456 88.2108C70.763 91.7899 70 95.626 70 99.5C70 103.374 70.763 107.21 72.2456 110.789C73.7281 114.368 75.901 117.62 78.6404 120.36C81.3797 123.099 84.6317 125.272 88.2108 126.754C91.7899 128.237 95.626 129 99.5 129L99.5 99.5V70Z" fill="#181818"/></svg>`;

export const LOGO_MARK = {
  viewBox: "69 69 61 61",
  body: `<circle cx="99.5" cy="99.5" r="28.7625" fill="none" stroke="currentColor" stroke-width="1.475" stroke-dasharray="3 3"/><path d="M99.5 70C95.626 70 91.7899 70.763 88.2108 72.2456C84.6317 73.7281 81.3797 75.901 78.6403 78.6404C75.901 81.3797 73.7281 84.6317 72.2456 88.2108C70.763 91.7899 70 95.626 70 99.5C70 103.374 70.763 107.21 72.2456 110.789C73.7281 114.368 75.901 117.62 78.6404 120.36C81.3797 123.099 84.6317 125.272 88.2108 126.754C91.7899 128.237 95.626 129 99.5 129L99.5 99.5V70Z" fill="currentColor"/>`,
};
