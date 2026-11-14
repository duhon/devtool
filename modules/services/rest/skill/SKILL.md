---
name: module-rest
description: "REST checks in the Services DevTools Console: the URL under #target, the HTTP method under #action, #metadata and #payload. Load before writing, editing or explaining REST requests."
---

# REST service

```text
#check Book example
#target
https://fakerestapi.azurewebsites.net/api/v1/Books/123
#action
PUT
#metadata
Content-Type: application/json
Accept: application/json
#payload
{"id": 123}
```

`#target` holds only the URL; a method written there is an error. The HTTP
method goes under `#action`: GET, POST, PUT, PATCH, DELETE, HEAD or OPTIONS.
Without `#action` the method is GET, even with a `#payload`. `#metadata`
uses one Name: value per line. `#payload` is optional request text.
Path parameters, enum suggestions and JSON body templates depend on matching
discovered metadata; consult the discovery topic, not a guessed UI button.
