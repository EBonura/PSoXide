#!/usr/bin/env node

import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import path from "node:path";

const API_BASE = "https://discord.com/api/v10";
const DEFAULT_GUILD_ID = "1531283189308588213";
const EXPECTED_GUILD_NAME = "PSoXide";
const AUDIT_REASON = "PSoXide idempotent server provisioning";

const Permission = {
  KICK_MEMBERS: 1n << 1n,
  BAN_MEMBERS: 1n << 2n,
  VIEW_AUDIT_LOG: 1n << 7n,
  SEND_MESSAGES: 1n << 11n,
  MANAGE_MESSAGES: 1n << 13n,
  CREATE_PUBLIC_THREADS: 1n << 35n,
  CREATE_PRIVATE_THREADS: 1n << 36n,
  SEND_MESSAGES_IN_THREADS: 1n << 38n,
  MODERATE_MEMBERS: 1n << 40n,
};

const moderatorPermissions =
  Permission.KICK_MEMBERS |
  Permission.BAN_MEMBERS |
  Permission.VIEW_AUDIT_LOG |
  Permission.MANAGE_MESSAGES |
  Permission.MODERATE_MEMBERS;

const announcementWritePermissions =
  Permission.SEND_MESSAGES |
  Permission.CREATE_PUBLIC_THREADS |
  Permission.CREATE_PRIVATE_THREADS |
  Permission.SEND_MESSAGES_IN_THREADS;

const sleep = (milliseconds) =>
  new Promise((resolve) => setTimeout(resolve, milliseconds));

async function loadToken() {
  if (process.env.DISCORD_BOT_TOKEN?.trim()) {
    return process.env.DISCORD_BOT_TOKEN.trim();
  }

  const tokenFile = process.env.DISCORD_BOT_TOKEN_FILE;
  if (tokenFile) {
    return (await readFile(tokenFile, "utf8")).trim();
  }

  throw new Error(
    "Set DISCORD_BOT_TOKEN or DISCORD_BOT_TOKEN_FILE before running.",
  );
}

const token = await loadToken();
const guildId = process.env.DISCORD_GUILD_ID ?? DEFAULT_GUILD_ID;

async function discord(method, route, body) {
  for (let attempt = 0; attempt < 5; attempt += 1) {
    const response = await fetch(`${API_BASE}${route}`, {
      method,
      headers: {
        Authorization: `Bot ${token}`,
        "Content-Type": "application/json",
        "X-Audit-Log-Reason": AUDIT_REASON,
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    });

    if (response.status === 429) {
      const rateLimit = await response.json();
      await sleep(Math.ceil((rateLimit.retry_after ?? 1) * 1000));
      continue;
    }

    if (!response.ok) {
      const detail = await response.text();
      throw new Error(
        `${method} ${route} failed (${response.status}): ${detail}`,
      );
    }

    if (response.status === 204) {
      return null;
    }

    return response.json();
  }

  throw new Error(`${method} ${route} remained rate-limited after retries.`);
}

const actions = [];
const record = (message) => {
  actions.push(message);
  console.log(message);
};

async function ensureCategory(channels) {
  let category = channels.find(
    (channel) => channel.type === 4 && channel.name === "PSoXide",
  );

  if (category) {
    return category;
  }

  category = channels.find(
    (channel) => channel.type === 4 && channel.name === "Text channels",
  );

  if (category) {
    category = await discord("PATCH", `/channels/${category.id}`, {
      name: "PSoXide",
      position: 0,
    });
    record("Renamed the default text category to PSoXide.");
    return category;
  }

  category = await discord("POST", `/guilds/${guildId}/channels`, {
    name: "PSoXide",
    type: 4,
    position: 0,
  });
  record("Created the PSoXide category.");
  return category;
}

async function ensureModeratorRole(roles) {
  let role = roles.find(
    (candidate) => !candidate.managed && candidate.name === "Moderator",
  );

  const desired = {
    name: "Moderator",
    permissions: moderatorPermissions.toString(),
    color: 0xe5534b,
    hoist: true,
    mentionable: false,
  };

  if (!role) {
    role = await discord("POST", `/guilds/${guildId}/roles`, desired);
    record("Created the Moderator role.");
    return role;
  }

  const needsUpdate =
    role.permissions !== desired.permissions ||
    role.color !== desired.color ||
    role.hoist !== desired.hoist ||
    role.mentionable !== desired.mentionable;

  if (needsUpdate) {
    role = await discord(
      "PATCH",
      `/guilds/${guildId}/roles/${role.id}`,
      desired,
    );
    record("Updated the Moderator role.");
  }

  return role;
}

async function ensureTextChannel({
  channels,
  category,
  moderator,
  name,
  topic,
  readOnly = false,
  position,
}) {
  let channel = channels.find(
    (candidate) =>
      (candidate.type === 0 || candidate.type === 5) &&
      candidate.name === name,
  );

  const permissionOverwrites = readOnly
    ? [
        {
          id: guildId,
          type: 0,
          allow: "0",
          deny: announcementWritePermissions.toString(),
        },
        {
          id: moderator.id,
          type: 0,
          allow: announcementWritePermissions.toString(),
          deny: "0",
        },
      ]
    : [];

  const desired = {
    name,
    type: 0,
    topic,
    parent_id: category.id,
    position,
    nsfw: false,
    rate_limit_per_user: 0,
    permission_overwrites: permissionOverwrites,
  };

  if (!channel) {
    channel = await discord("POST", `/guilds/${guildId}/channels`, desired);
    channels.push(channel);
    record(`Created #${name}.`);
    return channel;
  }

  const currentOverwrites = JSON.stringify(
    (channel.permission_overwrites ?? []).map(({ id, type, allow, deny }) => ({
      id,
      type,
      allow,
      deny,
    })),
  );
  const desiredOverwrites = JSON.stringify(permissionOverwrites);
  const needsUpdate =
    channel.topic !== topic ||
    channel.parent_id !== category.id ||
    channel.nsfw ||
    channel.rate_limit_per_user !== 0 ||
    currentOverwrites !== desiredOverwrites;

  if (needsUpdate) {
    channel = await discord("PATCH", `/channels/${channel.id}`, desired);
    const index = channels.findIndex((candidate) => candidate.id === channel.id);
    channels[index] = channel;
    record(`Updated #${name}.`);
  }

  return channel;
}

async function removeDefaultVoiceArea(channels) {
  const voiceCategory = channels.find(
    (channel) => channel.type === 4 && channel.name === "Voice channels",
  );
  if (!voiceCategory) {
    return;
  }

  const children = channels.filter(
    (channel) => channel.parent_id === voiceCategory.id,
  );
  const isUntouchedDefault =
    children.length === 1 &&
    children[0].type === 2 &&
    children[0].name === "General";

  if (!isUntouchedDefault) {
    console.warn(
      "Left the voice category unchanged because it is no longer the untouched Discord default.",
    );
    return;
  }

  await discord("DELETE", `/channels/${children[0].id}`);
  record("Removed the unused default voice channel.");
  await discord("DELETE", `/channels/${voiceCategory.id}`);
  record("Removed the unused default voice category.");
}

async function ensureServerSettings(guild, generalChannel) {
  const repositoryRoot = path.resolve(
    path.dirname(fileURLToPath(import.meta.url)),
    "../..",
  );
  const settings = {
    name: EXPECTED_GUILD_NAME,
    description:
      "The community for PSoXide: an open-source Rust stack for building, debugging and running games on the original PlayStation.",
    verification_level: 1,
    default_message_notifications: 1,
    explicit_content_filter: 2,
    system_channel_id: generalChannel.id,
    system_channel_flags: 15,
  };

  if (!guild.icon) {
    const icon = await readFile(
      path.join(repositoryRoot, "assets/branding/psoxide-app-icon.png"),
    );
    settings.icon = `data:image/png;base64,${icon.toString("base64")}`;
  }

  const needsUpdate = Object.entries(settings).some(([key, value]) => {
    if (key === "icon") {
      return true;
    }
    return guild[key] !== value;
  });

  if (!needsUpdate) {
    return;
  }

  await discord("PATCH", `/guilds/${guildId}`, settings);
  record("Applied the server description, icon, and safety settings.");
}

function autoModerationMatches(existing, desired) {
  const sameStringSet = (left, right) =>
    [...left].sort().join(",") === [...right].sort().join(",");
  const existingAction = existing.actions[0];
  const desiredAction = desired.actions[0];

  return (
    existing.event_type === desired.event_type &&
    existing.trigger_type === desired.trigger_type &&
    existing.enabled === desired.enabled &&
    existing.trigger_metadata.mention_total_limit ===
      desired.trigger_metadata.mention_total_limit &&
    existing.trigger_metadata.mention_raid_protection_enabled ===
      desired.trigger_metadata.mention_raid_protection_enabled &&
    existingAction?.type === desiredAction.type &&
    existingAction?.metadata?.custom_message ===
      desiredAction.metadata.custom_message &&
    sameStringSet(existing.exempt_roles, desired.exempt_roles) &&
    sameStringSet(existing.exempt_channels, desired.exempt_channels)
  );
}

async function ensureAutoModeration(moderator, announcements) {
  let rules;
  try {
    rules = await discord(
      "GET",
      `/guilds/${guildId}/auto-moderation/rules`,
    );
  } catch (error) {
    console.warn(`Skipped AutoMod: ${error.message}`);
    return;
  }

  const desiredRules = [
    {
      name: "Block obvious spam",
      event_type: 1,
      trigger_type: 3,
      trigger_metadata: {},
      actions: [
        {
          type: 1,
          metadata: {
            custom_message: "Please avoid spam and repeated messages.",
          },
        },
      ],
      enabled: true,
      exempt_roles: [moderator.id],
      exempt_channels: [announcements.id],
    },
    {
      name: "Limit mention spam",
      event_type: 1,
      trigger_type: 5,
      trigger_metadata: {
        mention_total_limit: 5,
        mention_raid_protection_enabled: true,
      },
      actions: [
        {
          type: 1,
          metadata: {
            custom_message: "Please avoid excessive mentions.",
          },
        },
      ],
      enabled: true,
      exempt_roles: [moderator.id],
      exempt_channels: [announcements.id],
    },
  ];

  for (const desired of desiredRules) {
    const existing = rules.find((rule) => rule.name === desired.name);
    if (existing) {
      if (autoModerationMatches(existing, desired)) {
        continue;
      }
      await discord(
        "PATCH",
        `/guilds/${guildId}/auto-moderation/rules/${existing.id}`,
        desired,
      );
      continue;
    }

    await discord(
      "POST",
      `/guilds/${guildId}/auto-moderation/rules`,
      desired,
    );
    record(`Created AutoMod rule: ${desired.name}.`);
  }
}

async function upsertPinnedMessage(channelId, heading, content, botId) {
  const messages = await discord(
    "GET",
    `/channels/${channelId}/messages?limit=100`,
  );
  let message = messages.find(
    (candidate) =>
      candidate.author.id === botId && candidate.content.startsWith(heading),
  );

  if (message) {
    if (message.content !== content) {
      message = await discord(
        "PATCH",
        `/channels/${channelId}/messages/${message.id}`,
        { content },
      );
      record(`Updated ${heading.replace(/^# /, "")}.`);
    }
  } else {
    message = await discord("POST", `/channels/${channelId}/messages`, {
      content,
      allowed_mentions: { parse: [] },
    });
    record(`Posted ${heading.replace(/^# /, "")}.`);
  }

  if (!message.pinned) {
    await discord("PUT", `/channels/${channelId}/pins/${message.id}`);
    record(`Pinned ${heading.replace(/^# /, "")}.`);
  }
}

async function ensureInvite(generalChannel) {
  const invites = await discord("GET", `/guilds/${guildId}/invites`);
  let invite = invites.find(
    (candidate) =>
      candidate.channel?.id === generalChannel.id &&
      candidate.max_age === 0 &&
      candidate.max_uses === 0 &&
      !candidate.temporary,
  );

  if (!invite) {
    invite = await discord(
      "POST",
      `/channels/${generalChannel.id}/invites`,
      {
        max_age: 0,
        max_uses: 0,
        temporary: false,
        unique: false,
      },
    );
    record("Created a permanent invite.");
  }

  return `https://discord.gg/${invite.code}`;
}

const bot = await discord("GET", "/users/@me");
const guild = await discord("GET", `/guilds/${guildId}`);

if (guild.id !== DEFAULT_GUILD_ID || guild.name !== EXPECTED_GUILD_NAME) {
  throw new Error(
    `Refusing to provision unexpected server ${guild.name} (${guild.id}).`,
  );
}

console.log(`Provisioning ${guild.name} as ${bot.username}...`);

let channels = await discord("GET", `/guilds/${guildId}/channels`);
const roles = await discord("GET", `/guilds/${guildId}/roles`);
const category = await ensureCategory(channels);
const moderator = await ensureModeratorRole(roles);

const channelDefinitions = [
  {
    name: "announcements",
    topic: "Official PSoXide news, releases, and project updates.",
    readOnly: true,
  },
  {
    name: "general",
    topic: "General conversation about PSoXide and PlayStation development.",
  },
  {
    name: "development",
    topic:
      "Technical discussion about the emulator, SDK, engine, editor, and disc tooling.",
  },
  {
    name: "help",
    topic: "Questions, troubleshooting, and help using or building PSoXide.",
  },
];

const provisionedChannels = {};
for (const [index, definition] of channelDefinitions.entries()) {
  provisionedChannels[definition.name] = await ensureTextChannel({
    channels,
    category,
    moderator,
    position: index,
    ...definition,
  });
}

await removeDefaultVoiceArea(channels);
await ensureServerSettings(guild, provisionedChannels.general);
await ensureAutoModeration(moderator, provisionedChannels.announcements);

const welcomeMessage = `# Welcome to PSoXide

PSoXide is an open-source PlayStation 1 development stack written in Rust. It combines an accuracy-focused emulator and debugger, a bare-metal SDK, a runtime engine, an asset editor, and disc tooling.

**Start here**
- [Try the web emulator](https://ebonura.github.io/PSoXide/)
- [Browse the source and documentation](https://github.com/EBonura/PSoXide)
- [Visit the project page](https://bonnie-games.itch.io/psoxide)

Use <#${provisionedChannels.general.id}> for conversation, <#${provisionedChannels.development.id}> for technical discussion, and <#${provisionedChannels.help.id}> when you need assistance.

PSoXide is pre-release software, so APIs, formats, and workflows may change.`;

const guidelinesMessage = `# Community guidelines

1. Be respectful and constructive.
2. Keep discussion relevant to PSoXide, PlayStation development, and related work.
3. Do not share copyrighted BIOS files, commercial disc images, piracy links, or other material you do not have permission to distribute.
4. Avoid spam, harassment, inflammatory behaviour, and unsolicited promotion.
5. Use GitHub as the source of truth for reproducible bugs, feature requests, and contributions.

Moderators may remove content or members when necessary to keep the community useful and welcoming.`;

await upsertPinnedMessage(
  provisionedChannels.announcements.id,
  "# Welcome to PSoXide",
  welcomeMessage,
  bot.id,
);
await upsertPinnedMessage(
  provisionedChannels.announcements.id,
  "# Community guidelines",
  guidelinesMessage,
  bot.id,
);

const inviteUrl = await ensureInvite(provisionedChannels.general);

console.log("");
console.log(
  actions.length === 0
    ? "Server already matched the desired configuration."
    : `Provisioning complete with ${actions.length} change(s).`,
);
console.log(`Invite: ${inviteUrl}`);
