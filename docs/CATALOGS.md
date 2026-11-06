# The shared catalogs

The second of the four common services (`docs/SERVICES.md`): data the applications read and do not own, the same for all of them. The routes are under `/api/v1/catalogs`; the contract is the `catalogs` tag of `docs/openapi.yaml`. The code is `src/catalogs.rs` and `src/api_catalogs.rs`.

| Catalog | What it holds | Where the data comes from | Named by |
|---|---|---|---|
| `materials` | laminates, prepregs, flex, metal core, PTFE: every value as its datasheet publishes it | built in: the database of the crate `eh3d-materials` of EH3D-COMMON, read-only | `id` (`manufacturer/name`) |
| `tools` | the tools of the tool store of EH3D-TOOL-MANAGER (drills, end mills, engravers) | uploaded by an administrator, JSON or CSV | `id` |
| `machine-profiles` | the profiles of the machines | uploaded | `id` |
| `footprints` | the library of footprints | uploaded | `name` |
| `symbols` | the library of symbols | uploaded | `name` |
| `components` | the components base of the PCB tool, format `phoenix-parts/1` | uploaded, JSON with `"format": "phoenix-parts/1"` | `id` |

## Who may do what

Everybody who is logged in reads (role viewer, scope `catalog:read`); only an administrator writes (scope `catalog:write`). The built-in `materials` is not written by anybody (`409 catalog_readonly`). A token with scopes does what the intersection of its role and its scopes allows.

## Reading

`GET /catalogs` lists the six catalogs with their version, number of items and tag. `GET /catalogs/{catalog}` answers a page:

| Query | What it does |
|---|---|
| `q` | keeps the items that have the text in any value (nested ones too), in any letter case |
| `filter` | conditions `field:value` separated by commas; the field is a path with dots for a nested one (`dk.value`); the value is compared in any letter case; a value that starts with `~` only has to be contained; on a list, one of the values has to match |
| `sort`, `desc` | the field to sort by (numbers as numbers, the rest as text), and the order |
| `limit`, `offset` | the page; `limit` is 50 and at most `catalog_page_max` |

The answer is `{"catalog","version","etag","total","offset","limit","items":[...]}`. `GET /catalogs/{catalog}/items/{item}` answers one item. An id with a slash (the ids of the materials are `manufacturer/name`) goes in the address with `%2F`.

**The tag.** The `ETag` is made from the content of the dataset (`"<catalog>-<16 characters of its SHA-256>"`), so it changes only when the content does, not at every upload. A client that keeps a copy sends it back in `If-None-Match` and gets `304` with no body when nothing changed.

## Writing

`PUT /catalogs/{catalog}` (administrator) replaces the whole dataset, as a new version, or does nothing: one item that does not fit refuses the whole upload (`422 bad_catalog`, with the item in the detail). The body is:

* JSON: a list of items, or an object with the list under `tools`, `profiles`, `footprints`, `symbols` or `parts` (`items` also works). The components base has to declare `"format": "phoenix-parts/1"`; its items are those of the format (`docs/PARTS.md` of EH3D-PHOENIX-PCB) and the server keeps them as they are.
* CSV (`Content-Type: text/csv`): a header line and a row per item; commas or, when the header has more of them, semicolons; quotes with `""` inside and line breaks inside quotes; the values are kept as text and an empty cell leaves the key out. The components base is not accepted as CSV here (its prices and offers need the JSON form).

Each item has to be an object with the field that names it (the last column of the table above): a text of up to 200 characters, unique in the dataset. The size is bounded by `catalog_max_mb` (`413`/`422 too_big`) and the number of items by `catalog_max_items`. The previous file stays beside the new one as `.bak`. `DELETE /catalogs/{catalog}` writes a new version with no items.

On the disk: `catalogs/<catalog>.json` in the data folder, written through a temporary file. Every upload is audited (who, the catalog and the version) and told on the stream as `catalog.changed` (`docs/MACHINE_PLUGINS.md`).

## What the server does not do

It does not interpret the items beyond the field that names them: what a material's `dk` means or what a part's `offers` mean is the business of the applications and of the formats of EH3D-COMMON. It does not fetch anything from a network and it does not check prices. The tool store, the machine profiles and the libraries are only as good as what the administrator uploads; nothing here has been tried with the real files of those applications yet.
