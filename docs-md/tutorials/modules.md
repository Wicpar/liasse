# Tutorial: module boundary

Modules let one package extend another package without silently sharing authority.

## Host package shape

A host owns a module space and exposes a narrow parent surface.

```hjson
$model: {
  companies: {
    $key: id
    id: text
    name: text
    plan: text

    modules: {
      $modules: {
        $expose: {
          company: {
            $view: "^ { id, name, plan }"
          }
        }
      }
    }
  }
}
```

The exposed `company` surface is what installed modules can import. The module cannot read arbitrary host fields unless the host exposes them.

## Module package shape

A module declares what it imports.

```hjson
$liasse: 1
$module: acme.company-card@1.0.0

$use: {
  company: { $from: parent }
}

$model: {
  card: {
    display_name: "= #company.name"
    tier: "= #company.plan"
  }
}
```

## Update rule

A module update is admitted only if structural rechecks pass. If an update would break imports, exposed interfaces, stored data, or delete-policy completeness, the update is rejected before a commit is admitted.

## Boundary rule

Nothing is implicit across modules. Views, mutations, limits, sources, refs, and delete behavior all cross a module boundary only through declared contracts.
