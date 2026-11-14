# Discovery and schemas

Discovery obtains metadata for completions; it does not run the check's normal
SQL, REST operation, GraphQL query or gRPC method. It still makes real metadata
requests using the configured connection and headers.

`console_context.schemaAvailable` reports that the service module holds a
discovered schema for the active check in the current application instance. False can mean discovery has not run, the
target/authentication changed, discovery is pending, or a previous attempt
failed. It does not prove the remote service lacks a specification.
`console_schema` asks the module for that schema (GraphQL introspection, REST
OpenAPI summary, OpenSearch mappings, MySQL tables and columns, MongoDB collections and
sampled fields, Artemis broker
names, gRPC services and message fields) and reads it with offset/limit paging. The Agent has
no tool to start or force discovery and cannot dispatch editor keystrokes or
click interface controls. Editing through workspace tools is not a substitute
for these user-driven discovery triggers.

## REST: what to tell the user

There is no Discover button. Alt+Space in the editor requests discovery for
the check at the cursor. The automatic triggers are:

1. Select the rest service tab and the relevant check. Its `#target` must
   contain the URL (the method goes in `#action`), and any authentication headers belong in `#metadata`.
2. In the URL, typing `/` triggers discovery for the prefix up to that slash.
   For an existing URL, delete and retype its slash immediately after the host
   or at the API mount boundary. Do not add an extra slash or execute the check.
   Merely pasting the full URL, focusing it or pressing Enter there is not
   itself a REST discovery trigger.
3. Alternatively, for a request with `#payload`, put the cursor on its payload content
   and press Ctrl+Space. If matching REST paths are missing, payload completion
   requests discovery for the check's target.
4. Watch the bottom status strip for Discovery, OK, WARNING or ERROR. Inspect
   Debug for probe addresses and transport/authentication failures.
5. Read fresh console_context, then console_schema if schemaAvailable is true.

For the `PUT` of `https://fakerestapi.azurewebsites.net/api/v1/Books/123`, a concrete
instruction is to retype the existing slash between the host and `api`, or
request Ctrl+Space in its `#payload`. Shift+Enter would run the PUT, not discovery;
do not suggest executing it just to obtain schema.

The REST module looks for OpenAPI 3.x or Swagger 2.0 at known locations beneath
the current prefix and its parents, including `/openapi.json`, `/v3/api-docs`,
`/swagger.json`, `/spec.json`, `/swagger/v1/swagger.json`, `/api-docs` and
`/rest/default/schema?services=all`. It uses the check's resolved headers.
It is not an arbitrary specification-URL importer or a web crawler.

REST schemas/attempts are cached by connection/prefix and resolved headers.
Repeating a trigger for an unchanged attempted key may not send another request;
do not promise a refresh. Switching to the correct mount/authentication changes
the context; reopening the application clears its in-memory discovery cache.
Opening or closing the Agent panel does not clear it.

An absent schema does not prevent ordinary REST requests. Report what the
application actually returned; do not claim a public API lacks OpenAPI merely
because no schema is cached. Without metadata, ask for documentation or a
representative response rather than inventing required fields or API behavior.

## Other services

Every service with discovery supports Alt+Space in its editor to request it.
GraphQL, OpenSearch and MongoDB discover like REST: on Ctrl+Space hints for a check
whose address has no cached schema yet. MySQL and gRPC also discover
automatically when the cursor leaves their connection sections. Use the
relevant check's connection and headers and watch the same bottom status strip.
MySQL discovery supplies database tables/columns; GraphQL uses introspection;
OpenSearch reads the cluster, its mappings and aliases; MongoDB lists the database's
collections and samples a few documents of the collection being edited; gRPC discovers
services, methods and message fields using its reflection/proto support. Authentication or server capabilities can limit what is available.
Splunk and Terminal do not have this schema discovery flow.
