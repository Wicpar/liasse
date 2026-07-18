//! The store-like handle a `subscribe` returns: current rows/scalar plus a change
//! callback fed by the frames the session folds through the wasm replica.
//!
//! It reconstructs no state — every read delegates to `WireClient` (the §12.2 source
//! of truth) and every change is a re-read after the session applied a frame. It
//! follows the readable-store contract: `subscribe(run)` calls `run` immediately with
//! the current snapshot and returns an unsubscribe function.

import { parseCloseReason, parseFrontier, parseRows, parseScalar } from "./boundary.js";
import type { ConnectError } from "./errors.js";
import type { Json, SubId, ViewState, ViewStatus, WireClientCore, WireRow } from "./types.js";

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
  /// The client-side bit the replica cannot know: the error a refused `view` POST
  /// carried. `undefined` unless the view was refused; cleared when the view is
  /// (re)opened. Everything else in the state is derived from the authoritative replica.
  private failure: ConnectError | undefined;

  constructor(sub: SubId, intent: ViewIntent, client: WireClientCore, closeFn: () => Promise<void>) {
    this.sub = sub;
    this.intent = intent;
    this.client = client;
    this.closeFn = closeFn;
    this.ready = Promise.resolve();
    this.failure = undefined;
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

  /// The subscription's lifecycle status (§12.2). `open`/`closed` are DERIVED from the
  /// authoritative replica — `open` once it has observed a frontier (the first
  /// `init`/`scalar`), `closed` once it has terminated — so they never disagree with the
  /// rows; only the client-side `failed` and the "not-loaded-yet" `pending` are tracked
  /// here. A `reset` clears the replica (no frontier), so the state falls back to
  /// `pending` until the re-opened view's fresh `init`, never a stale `open`.
  private status(): ViewStatus {
    if (this.client.isClosed(this.sub)) {
      return "closed";
    }
    if (this.failure !== undefined) {
      return "failed";
    }
    if (this.client.frontier(this.sub) !== undefined) {
      return "open";
    }
    return "pending";
  }

  /// A point-in-time snapshot of the whole view state.
  snapshot(): ViewState {
    return {
      sub: this.sub,
      status: this.status(),
      rows: parseRows(this.client.rows(this.sub)),
      scalar: parseScalar(this.client.scalar(this.sub)),
      frontier: parseFrontier(this.client.frontier(this.sub)),
      closed: this.client.isClosed(this.sub),
      closeReason: parseCloseReason(this.client.closeReason(this.sub)),
      error: this.failure,
    };
  }

  /// Begin a fresh `view` open attempt: clear any prior failure (back to `pending`,
  /// unless the replica already holds the view) and broadcast, so a reactive consumer
  /// sees `pending` when the shell re-opens after a `reset` rather than a stale state.
  /// Internal to the shell.
  beginOpen(): void {
    this.failure = undefined;
    this.notify();
  }

  /// Record that the `view` POST was refused: put the error IN the state (so the store's
  /// status becomes `failed` with the reason), broadcast the transition, and deliver the
  /// error to the error listeners. Internal to the shell.
  fail(error: ConnectError): void {
    this.failure = error;
    this.notify();
    this.deliverError(error);
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
