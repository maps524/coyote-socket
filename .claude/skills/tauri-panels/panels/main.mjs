// The main app window. Under DEV_URL it loads the Vite root, so its pathname
// is "/". The splashscreen loads "/splashscreen.html", so exclude that.
export default {
  name: "main",
  match: ({ url }) => !url.includes("splashscreen") && (url === "/" || url.endsWith("/")),
};
