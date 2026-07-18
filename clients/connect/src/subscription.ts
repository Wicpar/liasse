//! The store-like handle a `subscribe` returns: current rows/scalar plus a change
//! callback fed by the frames the session folds through the wasm replica.
//!
//! It reconstructs no state — every read delegates to `WireClient` (the §12.2 source
//! of truth) and every change is a re-read after the session applied a frame. It
//! follows the readable-store contract: `subscribe(run)` calls `run` immediately with
//! the current snapshot and returns an unsubscribe function.

import { parseCloseReason, parseFrontier, parseRows, parseScalar } from "./boundary.js";
import type { ConnectError } from "./errors.js";
import type { Json, SubId, ViewState, WireClientCore, WireRow } from "./types.js";

/// The parameters a subscription was opened with — retained so the session can reopen
/// it verbatim after a §12.2 `reset`.
export interface ViewIntent {
  readonly address: string;
  readonly params: Json | null;
  readonly window: Json | null;
  readonly auth: Json | null;
  readonly context: Json | null;
}

/// A listener over the subscription's state changes.
export type StateListener = (state: ViewState) => void;
/// A listener over the subscription's errors (a fault, or a failed (re)subscribe).
export type ErrorListener = (error: ConnectError) => void;

/// A live view subscription as the app sees it: a readable store plus lifecycle.
export class Subscription {
  /// The subscription id echoed on this view's frames.
  readonly sub: SubId;
  /// The parameters it was opened with (used by the session to reopen after a reset).
  readonly intent: ViewIntent;
  /// Resolves when the server has opened (or replaced) the view; rejects if it could
  /// not be opened. Awaiting it is optional — the store fills in as frames arrive.
  ready: Promise<void>;

  private readonly client: WireClientCore;
  private readonly closeFn: () => Promise<void>;
  private readonly stateListeners = new Set<StateListener>();
  private readonly errorListeners = new Set<ErrorListener>();

  constructor(sub: SubId, intent: ViewIntent, client: WireClientCore, closeFn: () => Promise<void>) {
    this.sub = sub;
    this.intent = intent;
    this.client = client;
    this.closeFn = closeFn;
    this.ready = Promise.resolve();
  }

  /// The rows the replica currently holds, in view order.
  get rows(): WireRow[] {
    return parseRows(this.client.rows(this.sub));
  }

  /// The scalar value, for a scalar/aggregate view (`null` otherwise).
  get scalar(): Json | null {
    return parseScalar(this.client.scalar(this.sub));
  }

  /// Whether the subscription has terminated (closed or reset).
  get closed(): boolean {
    return this.client.isClosed(this.sub);
  }

  /// A point-in-time snapshot of the whole view state.
  snapshot(): ViewState {
    return {
      sub: this.sub,
      rows: parseRows(this.client.rows(this.sub)),
      scalar: parseScalar(this.client.scalar(this.sub)),
      frontier: parseFrontier(this.client.frontier(this.sub)),
      closed: this.client.isClosed(this.sub),
      closeReason: parseCloseReason(this.client.closeReason(this.sub)),
    };
  }

  /// Subscribe to state changes (readable-store contract): `run` fires immediately
  /// with the current snapshot and on every later change. `onError` receives faults
  /// and (re)subscribe failures. Returns a function that detaches both listeners.
  subscribe(run: StateListener, onError?: ErrorListener): () => void {
    this.stateListeners.add(run);
    if (onError !== undefined) {
      this.errorListeners.add(onError);
    }
    run(this.snapshot());
    return () => {
      this.stateListeners.delete(run);
      if (onError !== undefined) {
        this.errorListeners.delete(onError);
      }
    };
  }

  /// End the subscription (`unsubscribe`, §12.2). The server closes the stream for it;
  /// the store then reports `closed`.
  unsubscribe(): Promise<void> {
    return this.closeFn();
  }

  /// Recompute and broadcast the snapshot — called by the session after a frame for
  /// this subscription applied. Internal to the shell.
  notify(): void {
    const state = this.snapshot();
    for (const listener of this.stateListeners) {
      listener(state);
    }
  }

  /// Deliver a handled error to this subscription's error listeners. Internal.
  deliverError(error: ConnectError): void {
    for (const listener of this.errorListeners) {
      listener(error);
    }
  }
}
