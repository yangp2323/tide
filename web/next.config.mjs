/** Build static assets; the Rust binary serves pages and all /api routes. */
export default {
  output: "export",
  trailingSlash: true,
  reactStrictMode: true,
  poweredByHeader: false,
  images: { unoptimized: true },
  // Keep development overlays out of the mobile layout during local development.
  devIndicators: false,
};
