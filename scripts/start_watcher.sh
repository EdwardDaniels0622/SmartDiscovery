#!/bin/zsh
set -eu

APP_HOME="/Users/will/Library/Application Support/smart-wallet-discovery"
cd "$APP_HOME"

exec "$APP_HOME/smart-wallet-discovery" watch \
  --employee '0xd029b13f2e562c77e2db3e5eda140cee79a44fc1:bignewsagencypro:TECH:ai|gemini|llm|model|arena|openai|anthropic|xai|grok|google:30' \
  --employee '0x4d891f7bbf1de5a2c74cb9d718dc64cb743a5382:Squanchy.:WEATHER:weather|temperature|hurricane|storm|rain|snow:120' \
  --employee '0xa80e3fe5e7a445fa047fe6de1e27f9a15217b94b:bin8888:FINANCE:fed|rates|cpi|gdp|oil|wti|gold|stock:120' \
  --employee '0xaaddfa107f870155547ed77d11b540b70dc2d3f1:yxj425:FINANCE:fed|rates|cpi|gdp|oil|wti|gold|stock:120' \
  --employee '0x5f45b60c29d6e4d55c0bfd8ddd39ad45c5e0a77a:0x5F45b60C29d6e4D5:FINANCE:fed|rates|cpi|gdp|oil|wti|gold|stock:60' \
  --employee '0x448861155279dbf833d041b963e3ac854599e319:Flipadelphia:FINANCE:fed|rates|cpi|gdp|oil|wti|gold|stock:180' \
  --employee '0xe36f5735f5bc12c36b361a599e8603d56f7ccd91:sameday1:FINANCE:fed|rates|cpi|gdp|oil|wti|gold|stock:180' \
  --employee '0x725fd0798eca95357696f2521dd1d4784162570c:Bonereaper1:CRYPTO:bitcoin|btc|ethereum|eth|solana|sol|crypto:120' \
  --employee '0x9f2fe025f84839ca81dd8e0338892605702d2ca8:surfandturf:SPORTS:nba|nhl|nfl|mlb|ufc|soccer|tennis|championship:30' \
  --employee '0xe8278d22bef444ede84505b7c0ec4e4001305209:matthewceo:TECH:ai|gemini|llm|model|arena|openai|anthropic|xai|grok|google:30' \
  --employee '0xd12f443b6a45225ae65d519a4bdef568d29ce85a:jijitmu:TECH:ai|gemini|llm|model|arena|openai|anthropic|xai|grok|google:60' \
  --employee '0x73ecf6a30d3c6538cd931ec13c9ed6eba944f5bb:YakubianWarlord:TECH:ai|gemini|llm|model|arena|openai|anthropic|xai|grok|google:30' \
  --employee '0x09e7ed40e867fb65a07ce105b19d38c0d70e2737:vivonzulul:FINANCE:fed|rates|cpi|gdp|oil|wti|gold|stock:120' \
  --employee '0xf54f8f4c925a8b8b445a4a6ec93012c6fb4e3374:edcrfvtgbujmik:CULTURE:twitter|x|post|tweet|album|movie|celebrity:180' \
  --employee '0xb2a3623364c33561d8312e1edb79eb941c798510:aekghas:POLITICS:trump|biden|election|senate|house|president|china:120' \
  --employee '0x5d0f03cf1243a3e21262d6cf844795afd9fff0ad:EB99999:POLITICS:trump|biden|election|senate|house|president|china:120' \
  --employee '0x6011655c4afb76f36dd1b08a137a1ba73466b31e:HighTempTation:WEATHER:weather|temperature|hurricane|storm|rain|snow:60' \
  --employee '0xb19a7dc9f616c4270d5170a59a36d30de3ae3808:CHANCEHAT23:CULTURE:twitter|x|post|tweet|album|movie|celebrity:180' \
  --no-weatherhk-auto-copy \
  --poll-seconds 10 \
  --heartbeat-seconds 3600 \
  --min-notional 100 \
  --max-entry-price 0.75
