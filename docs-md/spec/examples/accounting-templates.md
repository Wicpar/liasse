# Example: company-local accounting template modules

This example shows a company row with its own module space. Installed modules
import a parent-provided `company` surface and expose checked template rows
back as `ecriture_templates`.

## 1. Host app package

```json
{
  "$liasse": 1,
  "$app": "acme.accounting@1.0.0",
  "$model": {
    "companies": {
      "$key": "id",
      "$sort": ["name", "id"],
      "id": "text",
      "name": "text",
      "plan": "text",
      "internal_notes": "text",

      "templates": {
        "$key": ["module", "template"],
        "module": "text",
        "template": "text",
        "label": "text",
        "journal": "text",
        "lines": "json"
      },

      "$mut": {
        "import_template({ module: text, template: text })": ".templates + .modules[:m | m.$key == @module].ecriture_templates[:t | t.$key == @template] { module: m.$key, template: t.$key, label, journal, lines }"
      },

      "modules": {
        "$modules": {
          "$expose": {
            "company": { "$view": ". { id, name, plan }" }
          },
          "$interfaces": {
            "ecriture_templates": {
              "$key": "id",
              "$sort": ["label", "id"],
              "id": "text",
              "label": "text",
              "journal": "text",
              "lines": "json"
            }
          }
        }
      },

      "available_ecriture_templates": {
        "$view": ".modules::ecriture_templates { module: modules.$key, template: ecriture_templates.$key, id, label, journal, lines, $sort: [label, module, template] }"
      }
    }
  },
  "$data": {
    "companies": {
      "acme": {
        "id": "acme",
        "name": "Acme SAS",
        "plan": "fr-pcg",
        "internal_notes": "private host data"
      }
    }
  }
}
```

The `modules` field is inside each `companies` row, so each company owns one
module space such as `/companies/acme/modules`. The host exposes only the
`company` view to installed modules; `internal_notes` stays private.

## 2. Installed data-pack module

```json
{
  "$liasse": 1,
  "$module": "acme.fr_sales_templates@1.0.0",
  "$use": {
    "company": "$parent"
  },
  "$model": {
    "templates": {
      "$key": "id",
      "$sort": ["label", "id"],
      "id": "text",
      "label": "text",
      "journal": "text",
      "enabled": "bool",
      "lines": "json"
    }
  },
  "$data": {
    "templates": {
      "sale_invoice": {
        "id": "sale_invoice",
        "label": "Facture de vente",
        "journal": "VE",
        "enabled": "= #company.plan == 'fr-pcg'",
        "lines": [
          { "account": "411", "side": "debit", "amount": "'= total_ttc" },
          { "account": "707", "side": "credit", "amount": "'= total_ht" },
          { "account": "44571", "side": "credit", "amount": "'= tva" }
        ]
      }
    }
  },
  "$expose": {
    "ecriture_templates": ".templates[:t | t.enabled] { id, label, journal, lines }"
  }
}
```

Install it at `/companies/acme/modules/fr_sales`. Inside that instance,
`#company.plan` is Acme's plan. Installing the same package at another
company binds `#company` to that other row.

The `enabled` value in `$data` is a write-time expression evaluated once when
the seed row is applied. The formula-looking strings inside `lines` are
literal JSON strings: the leading `'` is the escape that stores `= total_ttc`
instead of evaluating it.

## 3. Aggregate view behavior

The host reads all exposed rows through:

```text
.companies["acme"].modules::ecriture_templates
```

The aggregate binds installed modules as `modules` and exposed template rows as
`ecriture_templates`. Its inferred identity is:

```text
modules.$key + ecriture_templates.$key
```

So `fr_sales/sale_invoice` and another module's `sale_invoice` are distinct
rows in `available_ecriture_templates`.

## 4. Importing one exposed template

The host mutation declared in §1 imports a selected exposed row into the
company-local `templates` collection:

```text
.companies["acme"].import_template({ module: "fr_sales", template: "sale_invoice" })
```
