# Permissions and sessions

Liasse treats identity and access as model data.

## Users

Users/accounts are keyed rows. The built-in `actor` names the current session's account ref.

## Roles

A role has `$members` and a surface. Membership decides who receives the surface; the surface decides what that user can read or call.

## Clients

The engine serves a manifest of granted views and mutation names. Untrusted clients send parameters to those names; they do not send arbitrary model expressions.
