# Deploy

A docker compose stack for one host:

| Service | Role | Exposed |
|---|---|---|
| `mosquitto` | MQTT broker, mTLS + ACL | `8883` |
| `timescaledb` | database | Docker network only |
| `demo-server` | the server binary, run in a plain Ubuntu image | via Traefik, `https://<host>/api/v1/` |
| `firmware-files` | nginx serving the signed firmware images | via Traefik, `https://<host>/fw/` |
| `traefik` | TLS termination with Let's Encrypt | `80`, `443` |

`mqtt.demo.example` is a placeholder. Replace it in `docker-compose.yml`,
`demo-server.env` and the broker certificate.

## Layout on the host

Copy this directory to the host, then add the git-ignored parts:

```
.env                           from .env.example
demo-server/demo-server        release binary (cargo build --release -p demo-server)
demo-server/demo-server.env    from demo-server.env.example
demo-server/config/            ca.pem, demo-server.cert, demo-server.key
mosquitto/certs/               ca.pem, broker.cert, broker.key
firmwares/                     signed .bin.sig.b64 images
traefik/acme.json              empty file, chmod 600
```

Then `docker compose up -d`.

- Build the binary on a machine with the same glibc as `ubuntu:24.04`, or
  change the image.
- The broker key must be readable by the mosquitto user in the container
  (uid 1883): `chown 1883 broker.key && chmod 600 broker.key`.
- `POSTGRES_PASSWORD` is baked into the database volume on first start.
  Rotating it later also needs an `ALTER USER`.

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
