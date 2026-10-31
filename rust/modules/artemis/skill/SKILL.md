---
name: module-artemis
description: ActiveMQ Artemis checks in the Services DevTools Console: QUEUES, BROWSE, SEND and RECEIVE through the broker's Jolokia console API, #options and message bodies. Load before writing, editing or explaining Artemis requests.
---

# Artemis service

Apache ActiveMQ Artemis through its Jolokia management API, the broker's web
console (port 8161), not a messaging protocol. `#endpoint` is a command and
the console URL with the login: `http://user:password@host:8161/<queue>`
(`https://` if the console uses TLS). The path is one queue name,
percent-encoded where needed; QUEUES takes no path. The login goes out as
Basic auth and needs a management role; Jolokia is at `/console/jolokia`
unless `Jolokia-Path` in `#options` says otherwise.

- QUEUES lists queues with their counters: queue, address, routing-type,
  messages, delivering, scheduled, consumers, added, acked, killed, expired,
  durable, paused. `Filter:` keeps queue names containing the text; Count
  defaults to 100.
- BROWSE shows messages without consuming them: message-id, timestamp, body,
  properties and headers (the broker's message fields: priority, durable,
  redelivered, expiration, …). `Selector:` is a JMS selector
  (`tenant = 't1'`); Count defaults to 10. Messages out with consumers are not
  browsable. A DLQ or ExpiryQueue message shows where it came from in its
  `_AMQ_ORIG_*` properties.
- SEND puts a text message with `#body` on the queue; `#headers` become string
  properties, `Durable: false` makes it non-persistent. Returns the message ID.
- RECEIVE removes messages from the queue and returns them (Count default 1,
  optional Selector). Management has no consumer: it browses, then removes the
  messages it could show; one that another consumer got first is skipped.

      #check failed orders
      #endpoint
      BROWSE http://admin:admin@localhost:8161/DLQ
      #options
      Count: 20
      Selector: _AMQ_ORIG_ADDRESS = 'orders'

The broker returns at most `management-message-attribute-size-limit`
characters of a body (256 by default, set in broker.xml): BROWSE warns about
cut bodies, and RECEIVE leaves such messages in the queue rather than lose the
rest. Bytes bodies are shown as UTF-8 text or hex. Count is 1-1000; the result
warns when the queue holds more. Columns-Type and Columns-Size work on the
result columns. Debug lists the Jolokia operations (passwords masked).
