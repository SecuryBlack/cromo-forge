# CromoForge — TODO / Definición

> **Estado:** solo diseño. No hay código. Este documento recoge las decisiones cerradas
> en la sesión del 2026-08-21 y lo que queda por cerrar antes de empezar.

- **Nombre del producto:** CromoForge
- **Binario:** `cromo-forge`
- **Servicio:** `cromoforge` (Linux) / `CromoForge` (Windows)
- **Licencia:** Apache 2.0 (igual que OxiPulse y Nexus Agent)
- **Repo previsto:** `securyblack/cromo-forge`
- **Dominio:** disponible, reservado

Cuarto miembro de la familia de agentes SB: OxiPulse (métricas), FerroSentry (seguridad),
CupraFlow (red/balanceo), Nexus Agent (túnel/orquestación) y **CromoForge (despliegue)**.

---

## Decisión de partida: agente hermano, no un módulo de Nexus

`nexus-agent/README.md` promete CI/CD, pero en `src/` no existe módulo de deploy y
`bollard`/`git2`/`octocrab` no están en su `Cargo.toml`. Esa promesa se cumple con un
agente separado, no metiéndola dentro de Nexus.

Motivos:

1. **Blast radius** — un bug de deploy no puede tumbar el túnel, que es lo que mantiene
   viva la telemetría y el acceso remoto.
2. **Ciclo de release** — deploy va a iterar mucho más rápido; Nexus debe ser aburrido.
3. **Footprint** — no engordar un binario que se instala en todos los hosts, incluso los
   que nunca despliegan.
4. **Valida la Fase 4 de Nexus** — el contrato genérico multi-agente con un caso real.

**Nexus solo aporta:** transporte (`CommandRequest`/`CommandResponse` sobre el túnel Conduit),
descubrimiento en el `registry`, y stream de eventos hacia la nube. Cero lógica de despliegue.

---

## Decisiones cerradas

### 1. Build — el artefacto es el contrato

> **El artefacto es una imagen OCI en un registry. CromoForge en producción solo hace
> `pull` + `up` + healthcheck + rollback.**

Construir en el host de producción contradice la premisa de la familia (Rust, footprint
mínimo) y es el fallo conocido de Coolify/Dokploy: un `docker build` que se come la RAM
del VPS y tumba la app que ya corría.

Con el contrato de imagen fijado, el runner de producción es solo descarga de capas +
socket de Docker. El *builder* pasa a ser un **rol separable del mismo binario**:

| `builder` | Quién lo usa | Implica |
|---|---|---|
| `local`  | OSS, homelab, servidor gordo | Construye en el mismo host. Riesgo asumido conscientemente. |
| `remote` | Cliente SB con volumen | Otro nodo suyo (posible provisión vía `hetzner_client.py`). El código nunca sale de su infra. |
| `cloud`  | Default SB gestionado | Build farm de SB. Cero infra para el cliente. Palanca de monetización (minutos de build). |

**Orden de implementación:** `cloud` primero (hace la demo tipo Railway y valida el contrato
de imagen) → `local` (casi gratis después: mismo código sin salto de red) → `remote` (`local` + auth).

**Coste asumido:** una build farm de SB significa custodiar código fuente de clientes.
Mitigación obligatoria: builders efímeros (VM/contenedor por build, destruido al terminar),
sin persistencia entre tenants, caché de capas por-tenant cifrada, y la política de secretos de abajo.

### 2. Control — reconciliador, con fuente pluggable

Push vs poll es falsa dicotomía y bifurcaría el producto entre SB y OSS.

> **CromoForge es un reconciliador:** lee *estado deseado* → compara con *estado observado*
> → converge. La **fuente** del estado deseado es un plugin.

| `source` | Comportamiento |
|---|---|
| `securyblack` | Llega por el túnel Conduit de Nexus (outbound 443, nada que abrir). Converge en segundos. |
| `git` | GitOps puro, poll de un repo con el manifiesto. Cero dependencia de SB. **Modo OSS.** |
| `local` | `cromoforge.toml` en disco + CLI. Deployer simple en un VPS. |

La lógica de despliegue es idéntica en los tres; solo cambia el origen del deseado.

Gana gratis la **autocuración**: si alguien reinicia el servidor o mata un contenedor a mano,
el bucle lo repone. Un sistema push puro no se entera.

**Regla de red:** el push nunca inicia conexión hacia el cliente. Nexus mantiene el túnel
saliente y CromoForge se registra en su `registry`. Misma postura de red que Oxi y Ferro;
no se abre una segunda vía de entrada al servidor del cliente.

### 3. Secretos — SB no puede leerlos

> **Los secretos de runtime nunca tocan el builder, y SB no puede descifrarlos.**

Dos clases separadas a propósito:

- **Build args** (versión de Node, flags): no sensibles, van al builder, se cachean.
  Hay que **bloquear activamente** que se cuele un secreto aquí — quedaría en las capas
  de la imagen. No basta con documentarlo.
- **Runtime secrets** (DB URL, API keys): se inyectan solo en el arranque del contenedor
  en producción. Nunca suben al builder, nunca entran en una capa.

**Modelo criptográfico:** cada instalación de CromoForge genera un par X25519, publica la
pública, y la nube **sella** los secretos contra ella. La DB de SB guarda ciphertext que
ni SB ni quien se lleve la base puede abrir. La UI permite escribir un secreto pero no
re-mostrarlo (write-only de verdad).

Esto es **distinto** de `api-internal/crypto_utils.py` (Fernet simétrico), que es correcto
para lo suyo: tokens de proveedores que la API tiene que usar en salida.

**Coste asumido:** si el cliente pierde el servidor, pierde los secretos. Hay que ofrecer
rotación y avisarlo con claridad.

### 4. Alcance v1 — "como Railway", recortado

**Fuera de CromoForge por diseño (son de otro agente):**

- **TLS y enrutado** → CupraFlow (o Traefik, que ya se usa). CromoForge *declara* rutas;
  otro las consume. Mezclarlo acaba en dos agentes peleándose por el 443.
- **Logs y métricas de la app** → OxiPulse extendido a logs. CromoForge emite eventos de
  *despliegue* (empezó, construyó, arrancó, falló, rollback), no el stdout de la app.

| Entra en v1 | Fuera (v2+) |
|---|---|
| Deploy desde imagen OCI (`pull` + reconcile) | Bases de datos gestionadas |
| Buildpack desde repo Git (nixpacks — ya usado en api-internal) | Multi-servicio con dependencias |
| Rollout con healthcheck y **rollback automático** | Preview environments por PR |
| Secretos sellados (X25519) | Escalado horizontal / réplicas |
| Estado y eventos de deploy hacia la nube | Volúmenes persistentes gestionados |

**Recorte v1:** una app, un contenedor, un servidor. Sin réplicas, sin multi-nodo.

**Pieza a clavar:** el rollback automático. Es lo que separa "un script de deploy" de
"un producto en el que confías", y es donde Coolify flojea.

---

### 5. CromoForge nace sobre `sb-agent-core` (añadido 2026-08-21)

Ver `../sb-agent-core/TODO.md`. CromoForge no reimplementa config, logging, wrapper de
servicio ni updater: los consume de `sb-agent-core`.

**Es el primer consumidor a propósito.** Al ser greenfield, valida la API del crate compartido
antes de retrofitear los otros cuatro agentes. Si `core` está mal diseñado, te enteras con un
agente, no con cinco.

Gana además el **status socket** de `core`, que le da `cromo-forge status` y `cromo-forge top`
(build → push → pull → healthcheck → live) casi gratis. Esa TUI es la mejor demo de la
experiencia tipo Railway que vamos a tener.

**Aviso sobre el proto:** `nexus-agent/proto/tunnel/v1/tunnel.proto` **ya contiene**
`DeployCommand` / `GitPullAction` / `DockerBuildAction` / `DockerComposeAction`, diseñados con
supuestos **contrarios** a los de este documento (comandos imperativos, build en el host,
Compose, sin secretos). **No reutilizarlos tal cual** — hay que reemplazar `DeployCommand` por
un mensaje de estado deseado. `DeployLog` y `DeployStatus` sí sirven casi enteros.

---

## Abierto — decidir antes de escribir código

- [ ] **Registry.** ¿GHCR (gratis, ya en uso, obliga a cuenta GitHub) o registry propio de SB
      (control total, coste de almacenamiento y operación)?
      *Inclinación: GHCR en v1; registry propio cuando el gestionado tenga tracción.*
- [ ] **Runtime en el host.** ¿Docker Compose (ya en uso, familiar, frágil para rollout sin
      downtime) o contenedores gestionados directamente vía API de Docker (más control,
      rollout limpio, más código)?
      *Inclinación: API de Docker, precisamente porque el rollback es la pieza clave y con
      Compose peleas contra la herramienta.*
- [ ] **Build farm.** ¿VM efímera por build sobre Hetzner (`hetzner_client.py`) o nodo fijo con
      contenedores aislados? *Efímero es más seguro y más caro por build.*

---

## Siguientes pasos

- [ ] Cerrar las tres decisiones abiertas de arriba.
- [ ] Escribir `README.md` + roadmap por fases en el formato de `nexus-agent` / `oxi-pulse`.
- [ ] Definir el contrato de estado deseado (manifiesto): mismo esquema para las tres `source`.
- [ ] Definir los mensajes de deploy sobre el Conduit Protocol
      (`nexus-agent/proto/tunnel/v1/tunnel.proto`) y cómo CromoForge se anuncia en el `registry` de Nexus.
- [ ] Esqueleto de repo igual que los hermanos: `sb-service.json`, `scripts/install.sh|ps1`,
      `.github/workflows/release.yml` cross-platform, auto-update vía `self_update`, `LICENSE` (Apache 2.0).
- [ ] Modelo de datos en `api-internal`: reutilizar `agents` (`agent_type = "cromoforge"`) y
      diseñar tablas de apps / deploys / secretos sellados.
