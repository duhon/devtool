# Artemis module release notes

## 4.0.0

- A new request format. `#endpoint` is only the broker: its web console URL
  (`http://localhost:8161`) or its Jolokia URL. `#action` holds the command, its `/queue`
  or `/address` and clauses, on one line or several:
  `BROWSE /DLQ WHERE _AMQ_ORIG_ADDRESS = 'orders' LIMIT 20`. A check in the old format
  stops with a message that says where each part moved.
- New commands: ADDRESSES and CONSUMERS list addresses and consumers, PUBLISH sends a
  message to an address, PURGE deletes messages, MOVE … TO /queue moves them, RETRY sends
  dead letters back to their queues, PAUSE and RESUME stop and restart delivery, and
  JOLOKIA sends `#body` to Jolokia as is.
- `LIMIT n` replaces the Count option. A list takes one condition, such as
  `QUEUES WHERE messages > 0`, or a name part: `QUEUES orders`. BROWSE, RECEIVE, PURGE and
  MOVE take a JMS selector in `WHERE`, which replaces the Filter and Selector options.
- SEND and PUBLISH take message properties as `SET name = 'value', …`. `#headers` are
  HTTP headers now, so you can log in with `Authorization: Basic …` instead of the URL.
  `Persistent: false` replaces `Durable: false`.
- Completion offers the commands at the start of `#action`, then the broker's queues or
  addresses, without the broker's own `$sys.` and `activemq.` ones; they are read again
  when `#endpoint` or `#headers` change. Then it offers the clauses and common message
  properties. The command, the queue, the clauses and the quoted values are colored.
- Works with Services DevTools 3.4.0 or newer.

## 3.2.1

- Settings → Modules shows the Artemis module's name and description before it is
  installed.

## 3.2.0

- The Artemis module is released apart from Services DevTools: install and update it in
  Settings → Modules. It works with Services DevTools 3.2.0 or newer.
- The module brings its Agent skill, `module-artemis`, with its request syntax and examples.
