//! Loading the wasm core in a browser. This is the ONE module that binds to a
//! specific generated package (`wasm/web`, built by `build-wasm.sh`); everything else
//! depends only on the `WireCore` interface, so a typecheck and the node integration
//! test need no build artifact and can inject either target's module.
//!
//! The specifier is non-literal on purpose: TypeScript then types the dynamic import
//! as `unknown` and does not try to resolve the generated file at compile time, so the
//! shell typechecks before the wasm is ever built. The cast to `WireCore` is the
//! structural contract the generated module satisfies (verified by the node test,
//! which drives the real `--target nodejs` build through this same interface).

import type { WireCore } from "./types.js";

/// The web-target package, relative to this module's COMPILED location
/// (`dist/src/wasm.js` → `../../wasm/web/...` resolves to `clients/connect/wasm/web`).
/// A bundler-based app typically imports the package directly and passes it as
/// `ConnectOptions.core` instead of relying on this default.
const WEB_CORE_URL = new URL("../../wasm/web/liasse_connect_wasm.js", import.meta.url);

/// The shape of the `--target web` module: the shared `WireCore` exports plus the
/// wasm-bindgen default initializer that fetches and instantiates the `.wasm`.
type WebModule = WireCore & { default: (input?: unknown) => Promise<unknown> };

/// Load and initialize the browser wasm core. `moduleUrl` overrides the default
/// location (e.g. a bundler-provided URL, or a CDN path).
export async function loadCore(moduleUrl: string | URL = WEB_CORE_URL): Promise<WireCore> {
  const specifier = typeof moduleUrl === "string" ? moduleUrl : moduleUrl.href;
  const module = (await import(specifier)) as WebModule;
  await module.default();
  return module;
}
