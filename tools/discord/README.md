# PSoXide Discord provisioner

`provision.mjs` creates and reconciles the small public PSoXide Discord server:

- `#announcements`
- `#general`
- `#development`
- `#help`
- one `Moderator` role
- verification, content filtering, and native AutoMod rules
- pinned welcome and community-guideline messages
- a permanent invite

It is idempotent: rerunning it updates the managed resources instead of creating
duplicates. It also removes Discord's untouched default voice channel and
category, but leaves them alone once they have been customized.

Use Node.js 18 or newer:

```sh
DISCORD_BOT_TOKEN_FILE=/path/to/token.txt \
DISCORD_GUILD_ID=1531283189308588213 \
node tools/discord/provision.mjs
```

Alternatively, set `DISCORD_BOT_TOKEN` in the environment. Never commit a bot
token to this repository.
