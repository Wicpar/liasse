# Liasse documentation

Liasse is a draft language and engine model for strongly checked application data: schemas, views, mutations, permissions, modules, history, blobs, and metered limits live in one package model.

This site is organized for two readers:

- **Product and application authors** should start with the guided material.
- **Engine implementers** should use the feature reference and normative spec.

## Recommended path

1. Read **Getting started** for the smallest useful app.
2. Read **Mental model** to understand rows, views, surfaces, commits, and modules.
3. Walk through the **Tasks tutorial**.
4. Use **Feature explanations** as the readable reference.
5. Use **Normative spec** when exact syntax matters.

## Current status

This repository contains the v0.4 Hjson/JSON draft. Canonical package artifacts are strict JSON. Hjson is authoring sugar only: it is parsed to the same strict JSON tree before validation, hashing, loading, or storage.

## What Liasse tries to simplify

A normal application spreads data rules across migrations, ORM classes, ad-hoc API handlers, permission checks, background jobs, audit logs, and billing counters. Liasse pulls those declarations into one checked model and lets the engine derive the API surface, storage lowering, live views, conflict checks, and history behavior.

The goal is not to hide data modeling. The goal is to keep the model visible, explicit, and replayable.
