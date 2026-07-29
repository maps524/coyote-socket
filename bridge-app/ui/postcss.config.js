// Empty on purpose. Without a config here PostCSS walks up the tree and finds
// the desktop app's Tailwind config at the repo root, whose plugins are not
// installed in this package. This app uses plain CSS.
export default { plugins: {} }
