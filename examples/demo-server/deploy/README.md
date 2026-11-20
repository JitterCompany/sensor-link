# Deploy

A docker compose stack for one host:

| Service | Role | Exposed |
|---|---|---|
| `mosquitto` | MQTT broker, mTLS + ACL | `8883` |
| `timescaledb` | database | Docker network only |
| `demo-server` | the server binary, run in a plain Ubuntu image | `https://<host>/api/v1/`, via Traefik or a host proxy |
| `firmware-files` | nginx serving the signed firmware images | `https://<host>/fw/`, via Traefik or a host proxy |
| `traefik` | optional: TLS termination with Let's Encrypt | `80`, `443` |

The host name lives in three places:

- `DEMO_HOST` in `.env`, used by the Traefik rules in `docker-compose.yml`
- `FIRMWARE_BASE_URL` in `demo-server.env`
- the broker certificate: `BROKER_HOSTS=<host>,mosquitto ../scripts/gen-dev-certs.sh`

The firmware gets it at build time, as `DEMO_MQTT_HOST`.

## Layout on the host

Copy this directory to the host, as the home directory of a dedicated user in
the `docker` group (db-admin reaches the database over SSH as that user, see
[../tools/db-admin/README.md](../tools/db-admin/README.md)). Then add the
git-ignored parts:

```
.env                           from .env.example
demo-server/demo-server        release binary (cargo build --release -p demo-server)
demo-server/demo-server.env    from demo-server.env.example
demo-server/config/            ca.pem, demo-server.cert, demo-server.key
mosquitto/certs/               ca.pem, broker.cert, broker.key
firmwares/                     signed .bin.sig.b64 images
traefik/acme.json              empty file, chmod 600 (Traefik only)
```

Then `docker compose up -d`.

- Build the binary on a machine with the same glibc as `ubuntu:24.04`, or
  change the image. The `Demo release build` workflow builds it on
  ubuntu-24.04. libpq is built in, so the image needs no extra packages.
  An artifact download loses the executable bit: `chmod +x` it.
- Mosquitto runs as uid 1883 and exits when it cannot read the broker key.
  The certs are mounted read-only, so set the owner on the host:
  `sudo chown 1883:1883 mosquitto/certs/broker.key mosquitto/demo.acl &&
  sudo chmod 600 mosquitto/certs/broker.key mosquitto/demo.acl`. For the ACL
  file it only silences a warning, for now.
- Without membership of the `docker` group, run `docker compose` with
  `sudo`. That group is root-equivalent on the host.
- `POSTGRES_PASSWORD` is baked into the database volume on first start.
  Rotating it later also needs an `ALTER USER`.

## HTTP side: Traefik or a reverse proxy on the host

The API and the firmware downloads need TLS. Pick one in `.env`:

- `COMPOSE_PROFILES=traefik` and `ACME_EMAIL`: Traefik takes ports 80 and
  443 and gets a Let's Encrypt certificate for `DEMO_HOST`.
- Otherwise the server and the firmware files listen on
  `127.0.0.1:$DEMO_HTTP_PORT` and `127.0.0.1:$DEMO_FIRMWARE_PORT`, and an
  existing proxy forwards to them. For nginx, inside the `server` block for
  `DEMO_HOST`:

```nginx
location /api/v1/ {
    proxy_pass http://127.0.0.1:8087;
}
# The trailing slash strips /fw/: the files sit at the root of that server
location /fw/ {
    proxy_pass http://127.0.0.1:8088/;
}
```

Either way MQTT is not proxied: Mosquitto terminates mTLS on 8883 itself.

## Certificates

[../scripts/gen-dev-certs.sh](../scripts/gen-dev-certs.sh) makes a throwaway
CA with a broker, a `demo-server`, an `admin` and device certificates, for
trying the stack. Use your own CA for anything real.

## MQTT identities and ACL

The certificate CN is the MQTT username and client id.
[mosquitto/demo.acl](mosquitto/demo.acl) restricts each identity:

| Identity (cert CN) | Access |
|---|---|
| any device | write `f/<CN>/#`, read `t/<CN>/#`, read `server/status` |
| `demo-server` | read `f/#`, write `t/#`, write `server/status` |
| `admin` | readwrite everything (`#`) |

- Devices publish on `f/<client-id>/...` and receive on `t/<client-id>/...`.
  The `%u` pattern rules keep a device out of other devices' topics.
- ACL denials are silent: publishes are dropped and subscriptions receive
  nothing.
- There is no revocation. A certificate stays valid until it expires. Add a
  CRL (mosquitto's `crlfile`) if you need to lock a device out.
- `demo-server` and `admin` are matched by CN alone, so a device certificate
  with one of those CNs would get their access. The provisioning tool
  uppercases UIDs, and the match is case-sensitive, so its normal flow cannot
  issue them. Anything else that signs with the same CA must refuse them, or
  give the server and admin certificates their own CA (mosquitto accepts
  several in `cafile`).

Check the broker from outside:

```sh
# fails: no client certificate
mosquitto_sub -h mqtt.demo.example -p 8883 --cafile certs/ca.pem -t '#'
# works
mosquitto_sub -h mqtt.demo.example -p 8883 --cafile certs/ca.pem \
  --cert certs/admin.cert --key certs/admin.key -t '#' -v
```

## Database access

The database port is not published. Use the container:

```sh
docker compose exec timescaledb psql -U demo -d demo
```

See [../database.md](../database.md) for the schema and the admin operations.
