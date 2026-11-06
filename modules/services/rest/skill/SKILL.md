---
name: module-rest
description: "REST checks in the Services DevTools Console: METHOD and URL under #endpoint, #headers and #body. Load before writing, editing or explaining REST requests."
---

# REST service

```text
#check Book example
#endpoint
PUT https://fakerestapi.azurewebsites.net/api/v1/Books/123
#headers
Content-Type: application/json
Accept: application/json
#body
{"id": 123}
```

The method and URL are together under `#endpoint`, not a separate bare method
line. Supported methods are GET, POST, PUT, PATCH, DELETE and HEAD. `#headers`
uses one Name: value per line. `#body` is optional request text.
Path parameters, enum suggestions and JSON body templates depend on matching
discovered metadata; consult the discovery topic, not a guessed UI button.
