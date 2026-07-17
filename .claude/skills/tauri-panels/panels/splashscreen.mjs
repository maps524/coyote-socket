// The splashscreen window (400x300, "/splashscreen.html"). Closes once the app
// finishes booting, so it's usually only present briefly after launch.
export default {
  name: "splashscreen",
  match: ({ url }) => url.includes("splashscreen"),
};
