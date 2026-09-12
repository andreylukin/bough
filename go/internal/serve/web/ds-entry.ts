// Design-system entry for /design-sync. The control room is an app, not a
// published package: this barrel is the library surface the converter
// bundles into window.BoughWeb, and the stories' relative imports resolve
// against it. Keep it in step with what src/stories/*.stories.tsx import.
export * from "./src/app";
export { default as App } from "./src/app";
export * from "./src/projects";
export * from "./src/skills";
export * from "./src/status";
export * from "./src/render";
