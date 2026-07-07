# Client protocol

The client protocol is derived from granted surfaces.

## Manifest

The engine serves the client a manifest of view names, mutation names, parameter schemas, and blob constraints. The manifest is the API contract.

## Calls

Clients call names with values. The engine evaluates the declared mutation, checks authority, checks the read basis, and admits or rejects the transaction.

## Live views

Views are live by default. Delivery windows are a client policy and engine delivery concern, not a change to the underlying model.
