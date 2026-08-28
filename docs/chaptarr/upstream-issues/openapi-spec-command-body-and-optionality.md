# Served openapi.json diverges from controller behavior (command body, parameter optionality, missing 400s)

Verified against v0.9.936 (develop @ 423b1bb).

## Summary

Thanks for Chaptarr, and for shipping a machine-readable spec at all — most of the
*arr family does not. We build a Discord request bot against the v1 API and tried to
generate our client from the served `openapi.json`. Three classes of divergence made
the generated client unusable, so we ended up hand-writing the request types and using
the spec only as a route inventory. All three look mechanical rather than intentional,
so we wanted to write them up in case they are cheap to fix.

## Observed behavior

**1. `POST /api/v1/command` request body does not describe what the endpoint accepts.**
The spec types the request body as `CommandResource` with `additionalProperties: false`,
and types `CommandResource.body` as the abstract `Command` base schema (whose properties
are almost all `readOnly`). The controller binds `CommandResource` only to read `name`,
then rewinds and re-reads the raw request stream and deserializes it into the concrete
command subtype that `name` resolves to. So the real payload is command-shaped JSON such
as `{"name":"BookSearch","bookIds":[1]}`. Generated validators reject `bookIds` because
it is not a `CommandResource` property and `additionalProperties` is `false`, and
generated models offer a `body` object that no caller ever sends. `name` is also
effectively required (`PostValidator.RuleFor(c => c.Name).NotBlank()`) but the spec does
not mark it so.

**2. Parameter optionality is wrong in both directions.**
- `GET /api/v1/book/lookup` declares `term` without `required`, but the controller
  returns `400 {"error":"term is required"}` when it is blank. A required parameter is
  published as optional.
- `PUT /api/v1/book/{id}` declares path `id` as `required: true, type: string`. The
  route template is `{id:int?}` and the action takes no `id` parameter at all — it reads
  the id from the body. An optional integer is published as a required string. (The
  sibling `GET`/`DELETE` on the same path correctly type `id` as `integer/int32`, so the
  three operations disagree with each other.)

**3. Controller-produced 400s are absent from the spec.**
`POST /api/v1/book` returns `400 "Cannot add book: missing upstream provider book/work
ID (Hardcover/Goodreads/OpenLibrary/GoogleBooks)."` from three places. The action only
carries `ProducesResponseType` attributes for 409 and 202, so the generated spec
documents 409 and 202 and no 400. A generated client treats the 400 as an unmodeled
protocol error rather than the actionable client-side validation failure it is.

## Source citations (file:line, verified)

- `src/Chaptarr.Api.V1/openapi.json:2752-2762` — `POST /api/v1/command` request body → `CommandResource`
- `src/Chaptarr.Api.V1/openapi.json:14135` — `CommandResource.body` → `$ref: Command`
- `src/Chaptarr.Api.V1/openapi.json:14044-14107` — `Command` base schema, mostly `readOnly`
- `src/Chaptarr.Api.V1/openapi.json:14193` — `CommandResource` `additionalProperties: false`
- `src/Chaptarr.Api.V1/Commands/CommandController.cs:59-73` — `[FromBody] CommandResource`, then `Request.Body.Seek(0)` and `STJson.Deserialize(body, commandType)`
- `src/Chaptarr.Api.V1/Commands/CommandController.cs:50` — `PostValidator.RuleFor(c => c.Name).NotBlank()`
- `src/Chaptarr.Api.V1/Commands/CommandResource.cs:17` — `public Command Body { get; set; }`
- `src/Chaptarr.Api.V1/openapi.json:2440-2445` — `term` declared with no `required`
- `src/Chaptarr.Api.V1/Books/BookLookupController.cs:48-53` — `term` blank → `BadRequest`
- `src/Chaptarr.Api.V1/openapi.json:1874-1881` — `PUT /api/v1/book/{id}` path `id`: required, string
- `src/Chaptarr.Http/REST/Attributes/RestPutByIdAttribute.cs:10` — route template `{id:int?}`
- `src/Chaptarr.Api.V1/Books/BookController.cs:1797-1798` — `UpdateBook([FromBody] BookResource)`, no `id` parameter
- `src/Chaptarr.Api.V1/Books/BookController.cs:1175-1178` — `ProducesResponseType` for 409 and 202 only
- `src/Chaptarr.Api.V1/Books/BookController.cs:1253`, `:1285`, `:1348` — the three `BadRequest` returns
- `src/Chaptarr.Api.V1/Books/BookController.cs:1386-1397` — `IsMissingUpstreamProviderBookId` predicate
- `src/Chaptarr.Api.V1/openapi.json:1384-1425` — `POST /api/v1/book` responses: 409 and 202, no 400

## Why it matters to API clients

Anyone generating a client from the spec gets a `POST /api/v1/command` model that cannot
express any real command, a `term` parameter their generator marks optional and therefore
omits from required-argument checks, and no 400 branch on the add path. We could not use
the generated output and instead vendored a route-inventory-only extract of the spec,
hand-writing the request and response types from the controllers. That works for us but
means we silently drift whenever a controller changes.

## Suggested fix

- Annotate the command POST with the shape it actually accepts, or document it as a free-form
  object (`additionalProperties: true` / `type: object`) so generators stop rejecting
  command-specific fields.
- Add `[Required]`/`[FromQuery(Name=...)]`-level metadata (or a `[BindRequired]`) to `term`
  so the generator emits `required: true`, and let `PUT /api/v1/book/{id}` declare the id
  as an optional `integer` matching `{id:int?}`.
- Add `[ProducesResponseType(typeof(string), 400)]` (or a shared error resource) to
  `AddBook`, so the documented response set matches the returns at `:1253`, `:1285`, `:1348`.
