---
name: module-artemis
description: "ActiveMQ Artemis checks in the Services DevTools Console: #endpoint, #action commands (QUEUES, ADDRESSES, CONSUMERS, BROWSE, RECEIVE, SEND, PUBLISH, PURGE, MOVE, RETRY, PAUSE, RESUME, JOLOKIA) with WHERE, LIMIT, SET and TO, #headers, #options and #body, through the broker's Jolokia console API. Load before writing, editing or explaining Artemis requests."
---

# Artemis service

Apache ActiveMQ Artemis through its Jolokia management API, the broker's web
console (port 8161), not a messaging protocol.

- `#endpoint` is only the broker: the web console URL, `http://user:password@host:8161`
  (`https://` with TLS), or its Jolokia URL when that is not at
  `/console/jolokia`. Log in in the URL or with an `Authorization: Basic …`
  header in `#headers`, not both; the user needs a management role.
- `#headers` are HTTP headers of every Jolokia call.
- `#action` is a command, its `/queue` or `/address`, then clauses, on one
  line or several. It defaults to QUEUES. A name with spaces or `=<>!,()` is
  quoted: `'my queue'`; a quote inside doubles: `'it''s'`.
- `#options`: `Persistent: false` (SEND, PUBLISH), Columns-Type, Columns-Size.
- `#body` is the message of SEND and PUBLISH, or the request of JOLOKIA.

Commands:

- QUEUES, ADDRESSES and CONSUMERS list what the broker has, 100 rows by
  default. They take one condition, `WHERE column op value` with `=`, `!=`,
  `<`, `>`, `CONTAINS` or `NOT CONTAINS` (`QUEUES WHERE messages > 0`), or a
  name part (`QUEUES orders`: names containing it; `CONSUMERS /orders`: that
  queue's consumers). An unknown column fails with the list of columns.
- BROWSE `/queue` shows messages without consuming them, 10 by default:
  message-id, timestamp, body, properties and headers (priority, durable,
  redelivered, expiration, …). Messages out with consumers are not browsable.
  A DLQ or ExpiryQueue message shows where it came from in its `_AMQ_ORIG_*`
  properties.
- RECEIVE `/queue` removes messages and returns them, 1 by default.
  Management has no consumer: it browses, then removes the messages it could
  show; one that another consumer got first is skipped.
- SEND `/queue` and PUBLISH `/address` put a text message with `#body` there.
  `SET name = 'value', …` adds string properties; values are always quoted.
  Returns the message ID.
- PURGE `/queue` deletes messages; MOVE `/queue` TO `/queue` moves them; both
  take an optional WHERE. RETRY `/DLQ` sends dead letters back to their
  queues. PAUSE and RESUME `/queue` stop and restart delivery. Each returns
  the count or the paused state.
- JOLOKIA sends `#body`, one Jolokia request object or an array, as is and
  shows each reply's status, value and error.

WHERE of BROWSE, RECEIVE, PURGE and MOVE is a JMS selector
(`tenant = 't1' AND AMQPriority > 4`). `LIMIT n` goes last, 1-1000; a list
warns when the broker has more.

      #check failed orders
      #endpoint
      http://user:password@localhost:8161
      #action
      BROWSE /DLQ
      WHERE _AMQ_ORIG_ADDRESS = 'orders'
      LIMIT 20

      #check new order
      #endpoint
      http://localhost:8161
      #headers
      Authorization: Basic YWRtaW46YWRtaW4=
      #action
      SEND /orders SET tenant = 't1', _AMQ_GROUP_ID = 'g1'
      #body
      {"id":7}

Properties are sent as strings, so `_AMQ_SCHED_DELIVERY` can't schedule a
message. The broker returns at most `management-message-attribute-size-limit`
characters of a body (256 by default, set in broker.xml): BROWSE warns about
cut bodies, and RECEIVE leaves such messages in the queue rather than lose the
rest. Bytes bodies are shown as UTF-8 text or hex. Debug lists the Jolokia
operations, with passwords and the Authorization header masked.
