# Permissions, users, and sessions

Status: standard. Identity, roles, surfaces, sessions, and the login system —
all modeled as ordinary Liasse data, with cryptographic verification kept at
the gateway boundary.

## 1. Identity is a ref; users are rows

Liasse ships no user table. It ships one rule: a commit's `$actor` must
resolve to an existing, non-disabled row before the commit is admitted. Where
accounts live is the app's model — historized, migratable, exportable, and
soft-disabled like anything else. Module actors are instance paths, with `$as`
carrying the human.

Authentication and authorization are separate rows: an **identity** proves who
(an SSO subject); an **account** is who acts. One identity may bind to several
accounts; the account is the actor, so switching accounts switches powers.

```hjson
"$types": { "provider": { "$enum": ["google", "azure", "franceconnect"] } },

"identities": {
  "$key": ["provider", "subject"],
  "provider": "provider",
  "subject": "text",
  "claims": "json"
},

"users": {
  "$key": "id",
  "id": { "$type": "uuid", "$default": "= uuid()" },
  "name": "text",
  "settings": "json",
  "disabled": { "$type": "bool", "$default": "= false" },
  "identities": { "$set": { "$ref": "/identities", "$on_delete": "restrict" } }
}
```

The reverse question — which accounts bind identity X — rides the engine's
reverse-reference index: `refs_to(identity_path)`. No model construct is
needed, and the identity is delete-protected by its explicit ref policy.

## 2. Roles: membership is a view, permission is a surface

A **surface** is a named view plus an optional `$mut` list. Read permission is
projection; write permission is the mutation list. A **role** bundles surfaces
and declares its members as a view — so membership is ordinary data, and
granting a role is an ordinary, permission-gated mutation.

```hjson
"$types": { "role": { "$enum": ["admin", "member"] } },

"companies": {
  "$key": "id",
  "id": "text",
  "name": "text",
  "plan": "text",

  "members": {
    "$key": "user",
    "user": { "$ref": "/users", "$on_delete": "restrict" },
    "role": "role"
  },

  "$mut": {
    "rename({ name: text })": ".name = @name",
    "set_role({ user: /users.$key, role: role })": ".members[@user].role = @role",
    "update_own_settings({ settings: json })": "/users[actor].settings = @settings"
  },

  "$roles": {
    "member": {
      "$members": ".members.user",
      "me": {
        "$view": "/users[actor] { id, name, settings }",
        "$mut": ["update_own_settings"]
      }
    },

    "admin": {
      "$members": ".members[:m | m.role == 'admin'].user",
      "company": {
        "$view": ". { id, name, plan, members: .members { user, role, user_name: /users[user].name }, subcompanies: }",
        "$mut": ["rename", "set_role"]
      }
    }
  },

  "subcompanies": { "$like": "^" }
}
```

Rules:

```text
actor is a builtin binding in surface views and mutations: the session's
  account row — row-level security is an ordinary filter (d.owner == actor)
surface entries may declare params; clients invoke names with typed values
  only — see CLIENT.md for the untrusted pipeline
roles are declared on rows, so location gives scope lexically
$members is any view producing account refs; expiry that depends on wall
  clock is gateway/session policy until a reader-time model is specified
enum-typed role fields make membership filters typo-proof at load
```

## 3. Granularity: project reads, call writes

**Reads are projections.** The admin surface above projects what admin sees;
private rows outside the projection are unnameable in that session. A surface
can expose an aggregate without the rows: `deposit_count: count(.deposits)`
computes with definer authority and the session sees outputs only.

**Writes are calls, with definer authority.** A session never patches raw
data; it calls mutations its surfaces list. The body of `set_role` or
`update_own_settings` executes with the authority of the row that declares it,
not with arbitrary client access. The surface grants the capability to call;
the definer scopes what the call touches.

**Recursion.** A pass-through of a `$like` collection whose shape is the
projection's own source applies the whole surface — view and `$mut` —
recursively. `subcompanies:` above makes Acme's admin the admin of the
subtree, minus whatever the projection excludes, all the way down; exceptions
are filters (`subcompanies: .subcompanies[:s | s.id != "hr"]:`). Each
subcompany also carries its own `$roles`, so local admins coexist with
inherited ones.

**The access rule.** A mutation is authorized if some ancestor of its target
(or the target itself) declares a role whose `$members` view contains the actor
and whose surface covers the operation. Authorization is re-verified before
commit admission — membership reads join the basis — so a revoked admin's
straggler proposal from an offline device is rejected deterministically.

## 4. Sessions and the login system

Fully modeled; the HTTP layer is thin (routes, OIDC crypto, cookies, random
token generation) and every state change is a mutation. At the root:
identities, flow state, and the account picker's intermediate state. Next to
the accounts: sessions — so admins can list and revoke their company's
sessions through an ordinary surface.

The gateway generates random `state`, `nonce`, PKCE verifier, login token, and
session token values. The model stores hashes or public protocol values; it
does not need a language-level randomness builtin.

```hjson
"auth": {
  "flows": {
    "$key": "state_hash",
    "state_hash": "text",
    "nonce_hash": "text",
    "verifier_hash": "text",
    "provider": "provider",
    "redirect": "text",
    "created_at": { "$type": "timestamp", "$default": "= now()" }
  },

  "logins": {
    "$key": "id",
    "id": { "$type": "uuid", "$default": "= uuid()" },
    "token_hash": "text",
    "identities": { "$set": { "$ref": "/identities", "$on_delete": "restrict" } },
    "expires": "timestamp"
  }
}
```

```hjson
"companies": {
  "auth": {
    "sessions": {
      "$key": "id",
      "id": { "$type": "uuid", "$default": "= uuid()" },
      "token_hash": "text",
      "actor": { "$ref": "/users", "$on_delete": "restrict" },
      "identities": { "$set": { "$ref": "/identities", "$on_delete": "restrict" } },
      "expires": "timestamp"
    }
  }
}
```

A **login** is proven identities without an actor (the picker state); a
**session** is proven identities plus a chosen account. Tokens are never
stored — only `hash(token)` — and the cookie carries
`<session-path>:<token>`, so lookup is one keyed read plus a hash compare.

The flow:

```text
/login      gateway creates state/nonce/PKCE values, inserts a flow row,
            redirects to IdP
/callback   verify OIDC at the gateway, upsert the identity row,
            refs_to(identity) lists bound accounts:
              0 -> app policy (invite / JIT-provision)
              1 -> mint session next to that account
              n -> insert a login row, render the picker
/pick       target account's identities must intersect the proven set
            (checked against data via refs_to), then mint the session
switching   same rule, no re-authentication: re-selection within the
            proven identity set
logout      delete the session row; admin revocation is the same delete
            through a surface
```

Per request:

```text
gateway   one keyed read; verify hash and expires — transport validity,
          wall clock is fine here
session   open_actor_session(path): resolve grants by the ancestor walk;
          the environment contains only the granted surfaces — everything
          else is absent, not forbidden
commit    actor membership re-verified before commit admission — authority,
          deterministic
```

Revoking an *account* rejects its in-flight proposals everywhere; revoking a
*session* stops its transport at the gateway. Deliberately asymmetric — and
session-row reads do not join every commit's basis, or touching `expires`
would reject unrelated writes. Commits record `$actor` (account) and `$via`
(session row) for audit.

## 5. Library surface

```text
store.system()                    embedder-only unrestricted session
store.open_actor_session(path)    capability-bounded session for an account
store.refs_to(path)               reverse-reference index lookup
$via                              commit field: the carrying session
```

## 6. Boundaries (v1)

```text
anonymous/public access   no $members representation (a role needs an
                          actor); the app's system session mediates public
                          reads
impersonation             a system-session capability with $as attribution,
                          not a modelable grant
identity lifecycle        app policy; Liasse supplies rows, refs, history,
                          and delete policies
wall-clock expiry         gateway/session policy in v1; reader-time model
                          expressions are deliberately absent
```
