# CLI

A command-line adapter for the same Rust registry and service clients used
by the native host and HTTP adapter. Shared parameter metadata lives in
`rust/contracts`; parsing, dispatch and streaming live in `rust/native`.

## Run

From the repo root:

    ./dev <service> --flag=value [--flag=value ...]

The source launcher requires Rust/Cargo and incrementally builds the binary.
The installed CLI needs neither Cargo nor Node.js:

```sh
"$HOME/Library/Application Support/ServicesDevTools/dev" rest --method=GET --endpoint=https://httpbin.org/get
```

Run with no service name (or an unknown one) to print the list of
services and their flags.

### Examples

    ./dev rest --method=GET --endpoint=https://httpbin.org/get

    ./dev graphql \
      --endpoint=https://countries.trevorblades.com/graphql \
      --query='{ country(code: "US") { name } }'

    ./dev grpc \
      --endpoint=grpcs://api.example.com:443/helloworld.Greeter/SayHello \
      --message='{"name":"world"}'

    ./dev mysql \
      --connection=mysql://user:password@127.0.0.1:3306/db \
      --sql='select * from table1 limit 10'

    ./dev terminal --endpoint="bash -c" --command="echo hello"

    ./dev terminal \
      --endpoint="ssh admin@host.example.com -i ~/.ssh/id_rsa" \
      --env='{"FOO":"bar"}' \
      --command="tail -f /var/log/app.log"

    ./dev splunk \
      --endpoint=https://splunk.example.com:8089 \
      --headers='{"Authorization":"Splunk abc123"}' \
      --search="index=main error" \
      -o from=-24h -o to=now

`--headers` and `--variables` (where applicable) take a JSON string, e.g.
`--headers='{"Authorization":"Bearer x"}'`. `proto` is optional for grpc
— omit it to discover the service via server reflection, same as the web
presentation. Splunk's search options (`from`/`to`/`app`/`count`) are all
one repeatable `-o key=value` flag (like curl's `-H` or docker's `-e`)
instead of a flag per option — pass as many `-o` as you need, e.g.
`-o from=-24h -o to=now -o app=search -o count=10`.

`terminal` streams: output is written to stdout/stderr as it arrives
instead of waiting for one final result, so a command that never exits
on its own (like `tail -f`) just keeps printing until you Ctrl+C —
which stops the command itself, not just this CLI.

## Tests

Run from the repo root: `cargo test -p services-native`.
