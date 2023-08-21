# misskey Discord Webhook Proxy

The proxy to send discord Webhook from misskey.

This proxy supports sending discord message from misskey note webhook.

## How to use (server)

1. build with `cargo build --release`
2. `./target/release/misskey-discord-webhook-proxy <listen addr & port>`

## How to use (proxy)

Set `http://<prox-server>/discord/<discord webhook id>/<discord webhook token>/misskey` as your misskey webhook url.
You can get discord webhook id and token from your webhook URL. If your webhook is like 
`https://discord.com/api/webhooks/1143087113424879627/adffadjkdfllandjkfkjadjklf`, `1143087113424879627` is webhook id and `adffadjkdfllandjkfkjadjklf` is token.
