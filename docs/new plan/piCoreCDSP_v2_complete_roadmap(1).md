# piCoreCDSP v2 – vollständige Roadmap und Checkliste

**Stand:** 16. August 2026  
**Projektname:** piCoreCDSP  
**Strategisches Designziel:** CamillaDSP 5  
**Zwischen-/Testplattform:** CamillaDSP 4.2  
**Aktuell stabile Referenz:** CamillaDSP 4.1.3 / CamillaGUI 4.1.0  
**Feste Architekturentscheidung:** CamillaDSP bleibt ein separater Prozess. `camillalib` wird nicht eingebettet.

---

# 1. Strategische Leitentscheidung

piCoreCDSP v2 wird nicht mehr als Weiterentwicklung der alten Controller-/ioplug-Architektur gebaut.

Stattdessen gilt:

- [ ] v2 wird als frischer, kleiner Core neu gebaut.
- [ ] CamillaDSP 5 ist das semantische Designziel.
- [ ] CamillaDSP 4.2 dient als Zwischen-/Testplattform.
- [ ] CamillaDSP 4.1.3 dient nur als stabile Referenz.
- [ ] Vor dem ersten v2-Produktrelease wird exakt eine CamillaDSP/CamillaGUI-Kombination als Produktionsstack gepinnt.
- [ ] Falls CamillaDSP 5 + passender CamillaGUI-/pyCamillaDSP-Stack rechtzeitig stabil veröffentlicht und hardwarevalidiert ist, wird v2 direkt auf 5 ausgeliefert.
- [ ] Andernfalls kann v2 zunächst auf 4.2 ausgeliefert werden.
- [ ] Es wird keine dauerhafte Multi-Version-Kompatibilität gepflegt.
- [ ] Entwicklungsadapter für alte Versionen werden nach einem Upgrade gelöscht.
- [ ] Git-Historie und Tags sind das Archiv.
- [ ] Eigene Workarounds werden ausschließlich als temporäre Compatibility-Bridges gebaut.
- [ ] Jeder Workaround erhält ein explizites Removal Criterion.
- [ ] Wenn CamillaDSP upstream eine Funktion übernimmt, wird eigener Code entfernt statt dauerhaft parallel weitergeführt.

Leitregel:

> **Design for CamillaDSP 5 semantics, ship only against a pinned and hardware-validated release stack.**

---

# 2. Feste Prozessarchitektur

CamillaDSP bleibt dauerhaft ein separater Prozess.

Ziel:

```text
Squeezelite / AirPlay / weitere ALSA Producer
                  │
                  ▼
            pcm.picorecdsp
                  │
                  ▼
              snd-aloop
                  │
                  ▼
        ┌──────────────────┐
        │   CamillaDSP     │
        │ separater Prozess│
        └────────┬─────────┘
                 ▲
                 │ WebSocket
          ┌──────┴──────┐
          │             │
      piCoreCDSP     CamillaGUI
         Rust
```

Feste Entscheidungen:

- [ ] `camillalib` wird nicht in piCoreCDSP eingebettet.
- [ ] piCoreCDSP besitzt keinen CamillaDSP-Engine-Code.
- [ ] piCoreCDSP benutzt keine internen `SharedConfigs`, `ControllerMessage`, `StatusStructs` oder andere CamillaDSP-Engine-Interna.
- [ ] CamillaDSP bleibt unabhängig start-/restart-/upgradebar.
- [ ] CamillaGUI und piCoreCDSP sprechen mit derselben öffentlichen CamillaDSP-Controlplane.
- [ ] WebSocket bleibt die Integrationsgrenze.
- [ ] ALSA bleibt die Audio-/Source-State-Grenze.
- [ ] Ein CamillaDSP-Crash darf nicht automatisch den piCoreCDSP-Prozess mitreißen.
- [ ] Ein piCoreCDSP-Crash darf CamillaDSP nicht mitreißen.
- [ ] CamillaDSP-Upgrades sollen nach Möglichkeit ohne Neubau des piCoreCDSP-Cores möglich bleiben.
- [ ] Eine zukünftige Library-Integration wird nur neu bewertet, falls Upstream ausdrücklich eine stabile First-Class Embedded API garantiert.

Warum diese Entscheidung:

- klare Prozessisolation,
- geringe Kopplung an CamillaDSP-Interna,
- einfacher Upgrade-Pfad,
- kleinerer piCoreCDSP-Build,
- gleiche öffentliche API für GUI und Coordinator,
- leichterer zukünftiger Rückbau eigener Workarounds.

---

# 3. Langfristiges Endziel

Der ideale Endzustand ist:

```text
Producer
   ↓
pcm.picorecdsp
   ↓
snd-aloop
   ↓
CamillaDSP
   ↓
DAC
```

piCoreCDSP besteht langfristig möglichst nur noch aus:

```text
Installer
+ ALSA Setup
+ Initialconfigs
+ Packaging
+ ggf. minimale Integrationslogik
```

Der Rust-Daemon ist kein Selbstzweck.

- [ ] Jede neue Upstream-Funktion wird darauf geprüft, ob eigener Rust-Code gelöscht werden kann.
- [ ] Kein Workaround darf ohne dokumentierten Löschpfad in den Core gelangen.
- [ ] Neue Upstream-Funktion ersetzt lokalen Code.
- [ ] Keine dauerhaften Doppelimplementierungen.

---

# 4. Eigentumsmodell

## 4.1 Benutzer / CamillaGUI besitzen

- [ ] Filter.
- [ ] Mixer.
- [ ] Pipeline.
- [ ] FIR-Dateien.
- [ ] Playback-Device.
- [ ] Resampler.
- [ ] DSP-/Output-Samplerate.
- [ ] `chunksize`.
- [ ] `target_level`.
- [ ] Volume.
- [ ] Mute.
- [ ] sonstige persistente DSP-Konfiguration.

## 4.2 ALSA besitzt

- [ ] Audiotransport.
- [ ] Producer Active/Inactive.
- [ ] aktuelle nominale Source-Samplerate.
- [ ] tatsächlich ausgehandeltes Format.
- [ ] tatsächlich ausgehandelte Channel-Zahl.

## 4.3 CamillaDSP besitzt

- [ ] Capture.
- [ ] Playback.
- [ ] DSP-Verarbeitung.
- [ ] Buffering.
- [ ] Clock Drift.
- [ ] Rate Adjust.
- [ ] Config-Validation.
- [ ] relative Pfade.
- [ ] `$samplerate$`-/Token-Auflösung.
- [ ] Device-Restarts.
- [ ] Processing-State.
- [ ] StopReason.
- [ ] Statefile.
- [ ] ConfigFilePath.
- [ ] Runtime-Config-Lifecycle.

## 4.4 Rust besitzt nur temporär

- [ ] Beobachtung von `snd-aloop`.
- [ ] Erkennung Active/Inactive.
- [ ] Erkennung der nominalen Source-Rate.
- [ ] Reconciliation zwischen ALSA und CamillaDSP.
- [ ] temporäre Source-Rate-Synchronisation.
- [ ] begrenztes Retry/Backoff.
- [ ] Diagnostik.
- [ ] Workarounds für noch fehlende Upstream-Funktionen.

Zentrale Regel:

> **User config wins on configuration. ALSA wins on source rate. CamillaDSP owns DSP lifecycle wherever upstream already can.**

---

# 5. Producer-unabhängiger Audioeingang

piCoreCDSP kennt keine konkreten Producer.

Beispiele:

```text
Squeezelite
AirPlay / Shairport Sync
weitere ALSA-Anwendungen
```

Alle Producer verwenden denselben Eingang:

```text
pcm.picorecdsp
```

Rust erhält niemals eine Producer-spezifische Abstraktion wie:

```rust
enum Producer {
    Squeezelite,
    AirPlay,
}
```

Stattdessen ausschließlich:

```rust
enum SourceState {
    Inactive,
    Active {
        sample_rate: u32,
    },
}
```

Checkliste:

- [ ] keine Squeezelite-spezifische Core-Logik.
- [ ] keine AirPlay-spezifische Core-Logik.
- [ ] keine Producer-Erkennung.
- [ ] keine Producer-Prioritätslogik in Rust.
- [ ] kein Audio-Mixing in piCoreCDSP.
- [ ] genau ein Producer besitzt den Ingress gleichzeitig.
- [ ] Producer-Arbitration bleibt außerhalb von piCoreCDSP.
- [ ] ein neuer ALSA-Producer darf keine Rust-Codeänderung benötigen.

---

# 6. ALSA-Ingress-Vertrag

Ziel:

```text
Producer
   ↓
pcm.picorecdsp
   ↓
ALSA plug
   ↓
snd-aloop
```

Zielkonfiguration:

```text
pcm.picorecdsp {
    type plug
    slave {
        pcm "hw:Loopback,1,0"
        format S32_LE
        channels 2
        rate unchanged
    }
}
```

Zielinvarianten:

```text
Format      = S32_LE
Channels    = 2
Samplerate  = unverändert
```

Checkliste:

- [ ] `rate unchanged` auf Ziel-ALSA verifizieren.
- [ ] S16 Producer → S32_LE testen.
- [ ] S24 Producer → S32_LE testen.
- [ ] S32 Producer → S32_LE testen.
- [ ] Stereo → Stereo testen.
- [ ] Mono → Stereo bewusst definieren.
- [ ] `route_policy` falls notwendig explizit setzen.
- [ ] 44.1 kHz testen.
- [ ] 48 kHz testen.
- [ ] 88.2 kHz testen.
- [ ] 96 kHz testen.
- [ ] 176.4 kHz testen.
- [ ] 192 kHz testen.
- [ ] sicherstellen, dass kein unbeabsichtigtes Rate-Resampling erfolgt.
- [ ] konkurrierendes Producer-Open testen.
- [ ] Verhalten beim Producer-Handover testen.

---

# 7. CamillaDSP-Transportvertrag

Eine piCoreCDSP-kompatible Config muss sinngemäß erfüllen:

```yaml
capture:
  type: Alsa
  device: "hw:Loopback,0,0"
  channels: 2
  format: S32_LE
  stop_on_inactive: true
```

Rust darf:

- [ ] lesen.
- [ ] validieren.
- [ ] Fehler loggen.
- [ ] Managed Mode bei Inkompatibilität aussetzen.

Rust darf nicht:

- [ ] Capture-Type reparieren.
- [ ] Capture-Device überschreiben.
- [ ] Channels automatisch ändern.
- [ ] Format automatisch ändern.
- [ ] `stop_on_inactive` automatisch setzen.
- [ ] User-YAML zurückschreiben.

Bei inkompatibler Config:

```text
Managed mode suspended
→ verständlicher Fehler
→ warten auf Benutzeränderung
```

---

# 8. Fünf getrennte Zustandswahrheiten

Das System hat keine einzelne „Config-Wahrheit“.

## 8.1 Source Transport State

Quelle:

```text
snd-aloop HCTL
```

liefert:

```text
Active
Rate
Format
Channels
```

ALSA ist die Wahrheit über die aktuelle Source.

## 8.2 DSP Process State

Quelle:

```text
CamillaDSP
```

relevante Zustände:

```text
Offline
Starting
Running
Paused
Stalled
Inactive
Failed
```

## 8.3 Applied Runtime Config

Quelle:

```text
GetConfig
GetPreviousConfig
```

Sie repräsentiert die zuletzt tatsächlich angewendete Benutzerentscheidung.

Beispiel:

```text
Datei:
gain = 0 dB

GUI:
gain = +6 dB
Apply
kein Save

Runtime:
gain = +6 dB
```

Während des Betriebs ist `+6 dB` maßgeblich.

## 8.4 Persistent Config State

Quelle:

```text
ConfigFilePath
Statefile
Config-Datei
```

Das ist die Bootstrap-/Reboot-Wahrheit.

Nicht automatisch die aktuelle Runtime-Wahrheit.

## 8.5 GUI Draft State

Noch nicht angewendete GUI-Änderungen.

Rust kennt diesen Zustand nicht und soll ihn nicht kennen.

---

# 9. Harte Config-Invarianten

- [ ] Rust schreibt niemals User-YAML.
- [ ] Keine `runtime.yml`.
- [ ] Keine Shadow-Config-Datei.
- [ ] Kein FIR-Pfad-Rewrite.
- [ ] Keine allgemeine Config-Adaptionsengine.
- [ ] `Save != Apply`.
- [ ] Save ohne Apply verändert den laufenden DSP nicht.
- [ ] Apply ohne Save ist legitime Runtime-Wahrheit.
- [ ] Normale Ratewechsel müssen Apply-ohne-Save erhalten.
- [ ] GUI Apply darf von Rust nicht zurückgerollt werden.
- [ ] Config-Wechsel dürfen von Rust nicht rückgängig gemacht werden.
- [ ] ConfigFilePath darf von der RuntimeConfig abweichen.
- [ ] Diese Abweichung ist kein Fehler.
- [ ] Disk-Datei ist nicht die normale Ratewechsel-Vorlage.

---

# 10. Reconciliation statt historischer State Machine

Grundmodell:

```text
Trigger
   ↓
aktuellen Gesamtzustand lesen
   ↓
Sollzustand bestimmen
   ↓
minimal notwendige Aktion
   ↓
settle
   ↓
aktuellen Gesamtzustand erneut lesen
   ↓
verifizieren
```

Trigger heute:

```text
ALSA HCTL Event
CamillaDSP Polling
Retry Timer
```

Trigger mit 4.2/5:

```text
ALSA HCTL Event
CamillaDSP SubscribeState
Retry Timer
langsamer Safety Reconcile
```

Leitregel:

> **Events carry no truth. They only cause a fresh snapshot.**

---

# 11. `snd-aloop` Source Observer

Rust liest:

```text
PCM Slave Active
PCM Slave Rate
PCM Slave Format
PCM Slave Channels
```

Verhalten:

- [ ] HCTL nonblocking öffnen.
- [ ] Events abonnieren.
- [ ] nach Event kurz debounce.
- [ ] danach vollständigen Snapshot neu lesen.
- [ ] Eventpayload nicht als endgültige Wahrheit verwenden.
- [ ] ca. 50 ms als initialen Debounce testen.
- [ ] langsamen periodischen Safety-Snapshot behalten.
- [ ] Format/Channels als Transport-Invarianten behandeln.
- [ ] keine Configmutation aufgrund Format/Channels.

---

# 12. Normaler Stop-Lifecycle

Primär:

```text
Producer stoppt
   ↓
PCM Slave Active = false
   ↓
CamillaDSP stop_on_inactive
   ↓
Capture wird freigegeben
```

Rust:

- [ ] führt im Normalfall keinen eigenen Stop aus.
- [ ] gibt CamillaDSP eine kurze Grace-/Settle-Phase.
- [ ] prüft danach den realen Zustand.
- [ ] Safety Stop nur bei unerwartetem Hängen.
- [ ] keine Configänderung bei normalem Stop.

---

# 13. Settled-State-Konzept

Keine Writes direkt auf rohe Zustandsänderungen.

Ablauf:

```text
State Event
   ↓
Debounce
   ↓
Fresh Read
   ↓
Transition noch aktiv?
   ├─ ja → später erneut lesen
   └─ nein → settled
```

Ein Zustand gilt als settled, wenn:

- [ ] CamillaDSP nicht mehr Starting/Stopping ist.
- [ ] ALSA Snapshot stabil ist.
- [ ] erwartetes `GetConfig`/`GetPreviousConfig` verfügbar ist.
- [ ] kein unmittelbar neueres Event eingetroffen ist.

Keine langen festen Sleeps.

Stattdessen:

- [ ] kurzer Debounce.
- [ ] Fresh Read.
- [ ] gegebenenfalls zweiter Fresh Read.

---

# 14. Runtime-Config-Priorität

## DSP Running/Paused

Quelle:

```text
GetConfig
```

## DSP nach normalem Stop Inactive

Quelle:

```text
GetPreviousConfig
```

## Cold Boot ohne Runtime-Historie

Quelle:

```text
Statefile / ConfigFilePath / Datei
```

Priorität:

```text
Active Runtime Config
        ↓
Previous Runtime Config
        ↓
Persistent File
```

---

# 15. Source-Rate-Policy

Genau zwei Fälle.

## 15.1 Kein Resampler

```text
devices.samplerate = current_source_rate
```

## 15.2 Resampler vorhanden

```text
devices.capture_samplerate = current_source_rate
```

User-owned bleibt:

```text
devices.samplerate
```

Rust verändert niemals:

- [ ] Resampler-Typ.
- [ ] Resampler-Qualität.
- [ ] DSP-Output-Rate.
- [ ] Chunksize.
- [ ] Target Level.

---

# 16. Aktueller Workaround – Rate Sync bei Running

Solange CamillaDSP upstream keinen persistenten Source-Rate-Override anbietet:

```text
Source active
DSP Running/Paused
   ↓
GetConfig FRESH
   ↓
Transportvertrag prüfen
   ↓
RateTarget bestimmen
   ↓
Rate korrekt?
   ├─ ja → nichts tun
   └─ nein → SetConfigValue(rate only)
   ↓
settle
   ↓
Fresh Read
   ↓
verify
```

Checkliste:

- [ ] nur exakt ein Rate-Feld ändern.
- [ ] keine Config langfristig cachen.
- [ ] lokales „Success“ erst nach Verifikation.
- [ ] Source Rate nach Write erneut prüfen.
- [ ] Runtime Config nach Write erneut prüfen.

---

# 17. Aktueller Workaround – Rate Sync nach Inactive

Solange CamillaDSP keinen Source-Rate-Override im Inactive-State anbietet:

```text
Source active
DSP settled Inactive
   ↓
GetPreviousConfig FRESH
   ↓
Transportvertrag prüfen
   ↓
genau ein Rate-Feld ändern
   ↓
SetConfig
   ↓
settle
   ↓
verify
```

Dadurch sollen erhalten bleiben:

- [ ] Filteränderungen.
- [ ] Mixeränderungen.
- [ ] Pipelineänderungen.
- [ ] Apply ohne Save.
- [ ] aktuell angewendete Config-Auswahl.

---

# 18. Cliffhanger A – kein nativer Runtime Source-Rate Override

Heute muss piCoreCDSP:

- [ ] Runtime-Config lesen.
- [ ] RateTarget bestimmen.
- [ ] Ratefeld ändern.
- [ ] bei Inactive PreviousConfig neu anwenden.

Dieser Code lebt ausschließlich in:

```text
rate_sync/
```

Beispielschnittstelle:

```rust
trait SourceRateSynchronizer {
    async fn ensure_source_rate(
        &self,
        source_rate: u32,
        snapshot: &DspSnapshot,
    ) -> Result<()>;
}
```

## Removal Criterion

Wenn CamillaDSP upstream einen Source-Rate-Override liefert, der:

- [ ] im Inactive-State gesetzt werden kann.
- [ ] Reload überlebt.
- [ ] SetConfig überlebt.
- [ ] GUI Apply überlebt.
- [ ] Config-Wechsel überlebt.
- [ ] Resampler korrekt behandelt.
- [ ] `$samplerate$` vor Tokenauflösung berücksichtigt.

dann:

```text
ConfigPatchRateSynchronizer
→ DELETE
```

---

# 19. Cliffhanger B – kein natives Loopback Rate Following

Heute beobachtet Rust:

```text
snd-aloop HCTL
→ Active
→ Rate
```

Dieser Code lebt ausschließlich in:

```text
source/alsa_loopback.rs
```

Interface:

```rust
trait SourceObserver {
    async fn snapshot(&self) -> Result<SourceSnapshot>;
    async fn next_trigger(&mut self) -> Result<()>;
}
```

## Removal Criterion

Wenn CamillaDSP upstream zuverlässig:

- [ ] Loopback Active erkennt.
- [ ] aktuelle Source-Rate selbst liest.
- [ ] Ratewechsel selbst verarbeitet.
- [ ] Capture bei Inactive freigibt.
- [ ] neue Rate anschließend selbst startet.

dann:

```text
source/alsa_loopback.rs
→ DELETE
```

Wenn CamillaDSP den kompletten Lifecycle übernimmt:

```text
rate_sync
+ große Teile reconcile
→ DELETE
```

und anschließend:

```text
Rust daemon
→ ggf. DELETE
```

---

# 20. Cliffhanger C – keine Config Revision / CAS

Heute können nahezu gleichzeitig auftreten:

```text
GUI Apply B
```

und:

```text
Rust SetConfig(A + new_rate)
```

Keine dokumentierte atomare Compare-and-Swap-Semantik.

Darum:

- [ ] Config unmittelbar vor Write fresh lesen.
- [ ] keine RuntimeConfig über lange Transitions cachen.
- [ ] unmittelbar vor Write Source erneut prüfen.
- [ ] Runtime-Fingerprint verwenden.
- [ ] nach Write Fresh Read durchführen.
- [ ] bei Abweichung lokale Arbeit verwerfen.
- [ ] von vorn reconciliieren.
- [ ] RateLimitExceeded/Disconnect nicht als halben Erfolg behandeln.

Produktgarantie:

> Concurrent GUI edits and source-rate transitions converge to the latest observable state. Exact simultaneous writes are not strictly transactional with the current API.

## Removal Criterion

Wenn upstream liefert:

- [ ] Config generation/revision.
- [ ] Compare-and-swap.
- [ ] optimistic concurrency.
- [ ] atomaren Source-Rate-Overlay.

dann kann Race-Mitigation stark reduziert oder gelöscht werden.

---

# 21. Cliffhanger D – `$samplerate$`-materialisierte Ressourcen

Problem:

```text
fir_$samplerate$.wav
```

kann beim Laden zu:

```text
fir_44100.wav
```

materialisiert werden.

Nachträgliches Rate-Patching kennt den ursprünglichen Token nicht mehr.

v2-Policy:

- [ ] `$samplerate$`-abhängige Ressourcen im Native-Rate-Modus zunächst nicht voll unterstützen.
- [ ] bekannte Fälle erkennen.
- [ ] nicht still falsch weiterlaufen.
- [ ] fail closed.
- [ ] verständliche Meldung.
- [ ] keine eigene Token-/Template-Engine bauen.
- [ ] separate Configs als Alternative dokumentieren.
- [ ] feste DSP-Rate + Resampler als Alternative dokumentieren.

## Removal Criterion

Wenn CamillaDSP einen echten Source-Rate-Override vor Token-/Pfadauflösung anbietet:

```text
token guard
→ DELETE
```

---

# 22. Cliffhanger E – DSP-State Polling

CamillaDSP 4.2/5 bietet `SubscribeState`.

Daher:

```rust
trait DspTriggerSource {
    async fn next_trigger(&mut self) -> Result<()>;
}
```

Entwicklung:

```text
4.1:
Polling

4.2/5:
SubscribeState
+
langsamer Safety-Reconcile
```

## Removal Criterion

Sobald unsere Produktionsbasis State Push stabil unterstützt:

```text
fast DSP state poller
→ DELETE
```

Der langsame Safety-Reconcile bleibt.

---

# 23. Feste Camilla-Abstraktionsgrenze

Der Reconciler kennt keinerlei WebSocket-Wireformat.

Semantische API:

```rust
trait CamillaControl {
    async fn version(&self) -> Result<Version>;

    async fn state(&self) -> Result<DspState>;
    async fn stop_reason(&self) -> Result<Option<StopReason>>;

    async fn active_config(
        &self
    ) -> Result<Option<ConfigDocument>>;

    async fn previous_config(
        &self
    ) -> Result<Option<ConfigDocument>>;

    async fn config_file_path(
        &self
    ) -> Result<Option<PathBuf>>;

    async fn set_config(
        &self,
        config: &ConfigDocument
    ) -> Result<()>;

    async fn set_config_value(
        &self,
        path: &str,
        value: Value
    ) -> Result<()>;

    async fn stop(&self) -> Result<()>;
}
```

Optional:

```rust
trait CamillaStateEvents {
    async fn subscribe_state(
        &self
    ) -> Result<StateStream>;
}
```

---

# 24. WebSocket v4/v5 Strategie

CamillaDSP 5 hat ein inkompatibles Wireformat.

Während Entwicklung:

```text
camilla/
├── mod.rs
├── protocol_v4.rs
└── protocol_v5.rs
```

Regeln:

- [ ] beide implementieren dieselbe semantische API.
- [ ] keine Versionschecks im Reconciler.
- [ ] keine Versionschecks im ALSA-Code.
- [ ] keine Wireformat-Details in `config_view`.
- [ ] keine direkte JSON-Abhängigkeit außerhalb des Adapters.
- [ ] 4.2 und next5 parallel als Canary testen.

Aber:

> Keine dauerhafte Multi-Version-Produktion.

Vor Produktrelease:

```text
genau ein Produktionsadapter
```

Nach endgültigem v5-Upgrade:

```text
protocol_v4.rs
→ DELETE
```

---

# 25. `camillalib` ausdrücklich nicht verwenden

Feste Entscheidung:

- [ ] keine `camillalib`-Dependency.
- [ ] CamillaDSP nicht in piCoreCDSP einbetten.
- [ ] `run_engine()` nicht aus piCoreCDSP starten.
- [ ] keine internen Engine-Channels benutzen.
- [ ] keine Shared-State-Strukturen aus CamillaDSP direkt verwenden.
- [ ] keine Kopplung an CamillaDSP-internes Rust-Schema.
- [ ] keine gemeinsame Failure-Domain.
- [ ] keine gemeinsame Build-/Feature-Matrix.

Warum:

- Prozessisolation bleibt erhalten.
- CamillaDSP kann unabhängig aktualisiert werden.
- CamillaGUI nutzt ohnehin WebSocket.
- beide Clients verwenden dieselbe Controlplane.
- piCoreCDSP bleibt klein.
- CamillaDSP-Library-API ist aktuell nicht als stabile First-Class API garantiert.
- spätere Upstream-Übernahme unserer Workarounds ist leichter.

Neu bewerten nur wenn Upstream später ausdrücklich garantiert:

- [ ] stabile Embedded Engine API.
- [ ] stabile SemVer-Regeln.
- [ ] stabile Lifecycle API.
- [ ] stabile State Subscriptions.
- [ ] stabile Config API.
- [ ] stabile Source-Rate API.
- [ ] klar dokumentierte Shutdown-/Thread-Semantik.

---

# 26. ConfigDocument bewusst schemaarm halten

Rust bildet nicht das komplette CamillaDSP-Configschema nach.

Interne Darstellung:

```text
ConfigDocument
```

als generischer YAML-/JSON-Baum.

Rust kennt nur benötigte Pfade:

```text
devices.samplerate
devices.capture_samplerate
devices.resampler

devices.capture.type
devices.capture.device
devices.capture.channels
devices.capture.format
devices.capture.stop_on_inactive
```

Nicht nachbauen:

- [ ] Filtermodelle.
- [ ] Mixer.
- [ ] Processors.
- [ ] Biquads.
- [ ] FIR-Schema.
- [ ] komplette v4/v5 Config-Strukturen.

---

# 27. CamillaDSP 5 als Designbasis

Von Anfang an:

- [ ] WebSocket gekapselt.
- [ ] State Events bevorzugen.
- [ ] keine Librarykopplung.
- [ ] Configschema minimal kennen.
- [ ] Build/Packaging separat halten.
- [ ] Produktionsversion exakt pinnen.
- [ ] keine automatischen Major-Upgrades.
- [ ] Upgrade auf 5 als bewusstes Produkt-Gate behandeln.
- [ ] GUI-Kompatibilität als gleichwertiges Release-Gate behandeln.

---

# 28. CamillaGUI Strategie

Keine piCoreCDSP-spezifische Fork.

- [ ] natives CamillaDSP-Statefile verwenden.
- [ ] Custom `on_get_active_config` entfernen.
- [ ] Custom `on_set_active_config` entfernen.
- [ ] Shadow-`active_config.yml` entfernen, falls nicht technisch zwingend.
- [ ] Rust beobachtet CamillaGUI niemals direkt.
- [ ] alle GUI-Änderungen werden ausschließlich über CamillaDSP beobachtet.
- [ ] GUI-Apply darf völlig unabhängig vom Rust-Controller erfolgen.
- [ ] kompatiblen CamillaGUI-/pyCamillaDSP-Stack vor v5-Release validieren.

---

# 29. GUI-Betriebszustände

## 29.1 Apply während Playback

```text
CamillaGUI
→ CamillaDSP
```

Rust:

- [ ] zunächst nichts verändern.
- [ ] Fresh Reconcile.
- [ ] nur Source-Rate bei Bedarf wieder angleichen.
- [ ] alle übrigen GUI-Änderungen erhalten.

## 29.2 Apply ohne Save

- [ ] RuntimeConfig gewinnt.
- [ ] normale Ratewechsel müssen sie erhalten.
- [ ] `GetConfig`/`GetPreviousConfig` bevorzugen.
- [ ] Datei nicht neu laden.

## 29.3 Save ohne Apply

- [ ] laufender DSP bleibt unverändert.
- [ ] Dateimodifikation löst keinen automatischen Reload aus.
- [ ] Rust darf Saved Draft nicht zum Runtime-Zustand machen.

## 29.4 Config A → Config B

- [ ] neueste angewendete Benutzerentscheidung gewinnt.
- [ ] Rust darf A niemals wiederherstellen.
- [ ] aktuelle Source-Rate wird auf B reconciliiert.

## 29.5 ConfigFilePath != RuntimeConfig

- [ ] legitimer Zustand.
- [ ] kein Repair.
- [ ] keine Warnung allein wegen dieser Abweichung.

---

# 30. Normaler Ratewechsel

Beispiel:

```text
44.1 kHz
   ↓
Producer Stop
   ↓
CamillaDSP Inactive
   ↓
PreviousConfig verfügbar
   ↓
neuer Producer 96 kHz
   ↓
Rust liest PreviousConfig fresh
   ↓
nur Rate anpassen
   ↓
SetConfig
   ↓
settle
   ↓
verify
```

Pflichttest:

```text
Disk:
gain = 0

Playback:
44.1 kHz

GUI:
gain = +6
Apply
NO SAVE

Source:
44.1 → 96 → 48
```

Erwartung:

```text
gain bleibt +6
Source Rate folgt 44.1 → 96 → 48
```

- [ ] muss bestehen.

---

# 31. Neue Source mit gleicher Rate

Auch:

```text
48 kHz
→ inactive
→ 48 kHz
```

ist ein neuer Source-Lifecycle.

- [ ] Active Generation erkennen.
- [ ] nicht nur `old_rate != new_rate` betrachten.
- [ ] DSP ggf. aus PreviousConfig neu starten.
- [ ] Rate nur ändern, falls nötig.

---

# 32. Concurrent Apply + Ratewechsel

Härtester Race-Fall.

Strategie:

```text
Trigger
→ settle
→ Source fresh
→ Config fresh
→ unmittelbar vor Write erneut prüfen
→ minimalen Rate-Write ausführen
→ settle
→ Fresh Read
→ verify
```

Regeln:

- [ ] keine alte Config blind erneut senden.
- [ ] keine Config Sekunden lang cachen.
- [ ] kein Retry mit stale Payload.
- [ ] bei Zweifel Reconcile von vorn.

---

# 33. Fehler- und Recovery-Modell

## WebSocket offline

- [ ] bounded backoff.
- [ ] reconnect.
- [ ] danach vollständiger Fresh Snapshot.

## DAC unavailable

- [ ] loggen.
- [ ] Retry mit aktueller RuntimeConfig.
- [ ] kein automatischer DAC-Wechsel.

## Invalid Config

- [ ] nicht reparieren.
- [ ] alte gültige RuntimeConfig bleibt maßgeblich, sofern CamillaDSP sie beibehält.
- [ ] sonst WaitingForUserFix.

## Incompatible Transport Config

- [ ] Managed Mode suspendieren.
- [ ] klare Fehlermeldung.
- [ ] auf neuen Apply warten.

## Stalled

- [ ] kurze Beobachtungsphase.
- [ ] nicht sofort Restart-Loop.
- [ ] Source erneut prüfen.
- [ ] StopReason prüfen.
- [ ] bounded retry.

## Rust Crash

- [ ] CamillaDSP läuft unabhängig weiter.
- [ ] Rust startet stateless neu.
- [ ] Source fresh lesen.
- [ ] DSP fresh lesen.
- [ ] Reconcile.

## CamillaDSP Crash

- [ ] Rust bleibt am Leben.
- [ ] CamillaDSP-Prozess neu starten bzw. externes Service-Management greifen lassen.
- [ ] unsaved RuntimeConfig kann verloren sein.
- [ ] das ist akzeptierte v2-MVP-Grenze.

## CamillaGUI Crash

- [ ] Audio unbeeinflusst.
- [ ] Rust unbeeinflusst.
- [ ] CamillaDSP unbeeinflusst.

---

# 34. Cold Boot

Bevorzugt:

```text
camilladsp -w -s statefile.yml
```

Nicht `--no_config` als feste Architekturvoraussetzung.

Zu testen:

- [ ] Boot ohne Producer.
- [ ] Statefile-Config vorhanden.
- [ ] `stop_on_inactive` führt zu sauberem Inactive.
- [ ] PreviousConfig anschließend verfügbar.
- [ ] Boot mit bereits aktivem Producer gleicher Rate.
- [ ] Boot mit bereits aktivem Producer anderer Rate.
- [ ] Startup CaptureError.
- [ ] Startup PlaybackError.
- [ ] fehlende ConfigFilePath.
- [ ] ungültige persistente Config.
- [ ] kein Statefile.

---

# 35. Kein normaler Disk-Config-Watcher

Nicht neu bauen:

```text
mtime watcher
inode watcher
file fingerprint → auto reload
```

Grund:

```text
Save != Apply
```

Runtime-Fingerprint nur für Race-Erkennung:

```text
hash(GetConfig)
hash(GetPreviousConfig)
```

Nicht für automatische Dateisynchronisation.

---

# 36. Empfohlene Modulstruktur

```text
src/
├── main.rs
├── reconcile.rs
│
├── source/
│   ├── mod.rs
│   └── alsa_loopback.rs
│
├── camilla/
│   ├── mod.rs
│   ├── protocol_v4.rs
│   └── protocol_v5.rs
│
├── rate_sync/
│   ├── mod.rs
│   └── config_patch.rs
│
├── config_view.rs
├── retry.rs
├── error.rs
└── logging.rs
```

Erwarteter zukünftiger Rückbau:

```text
protocol_v4.rs
→ DELETE

config_patch.rs
→ DELETE

alsa_loopback.rs
→ ggf. DELETE

reconcile.rs
→ ggf. stark kleiner
```

---

# 37. Reconcile-Pseudocode

```text
trigger

source = source_observer.snapshot()
dsp    = camilla.snapshot()

if source inactive:
    wait for stop_on_inactive
    if settled and DSP still running:
        safety recovery
    return

if source transport invalid:
    report
    suspend managed mode
    return

if DSP transitioning:
    reconcile later
    return

if DSP running or paused:
    cfg = GetConfig fresh
    validate transport
    rate_sync.ensure_source_rate(source.rate, cfg)
    verify later
    return

if DSP settled inactive:
    cfg = GetPreviousConfig fresh
          or bootstrap source if none
    validate transport
    rate_sync.start_with_source_rate(source.rate, cfg)
    verify later
    return

if DSP failed:
    classify
    bounded retry
    full fresh snapshot on every attempt
```

---

# 38. Upstream Capability Matrix

| Capability | 4.1 | 4.2 | 5 aktuell | eigener Workaround |
|---|---:|---:|---:|---|
| State Push Events | nein | ja | ja | Polling-Fallback |
| `stop_on_inactive` | ja | ja | ja | nutzen |
| `GetConfig` | ja | ja | ja | nutzen |
| `GetPreviousConfig` | ja | ja | ja | nutzen |
| `SetConfigValue` | ja | ja | ja | nutzen |
| persistenter Source-Rate Override | nein | nein | aktuell nein | `rate_sync/config_patch` |
| Source-Rate Override im Inactive-State | nein | nein | aktuell nein | PreviousConfig + SetConfig |
| natives aloop Rate Following | nein | nein | aktuell nein | `source/alsa_loopback` |
| Config Revision/CAS | nein | nein | aktuell nein | Fresh Reads + Verify |
| source-rate-aware Token Re-resolution | nein als Runtime API | nein | aktuell nein | Feature begrenzen |

Die Matrix ist kein Kompatibilitätsversprechen.

Sie dient ausschließlich dazu, eigene Workarounds gezielt löschen zu können.

---

# 39. Upstream Removal Matrix

| Upstream-Funktion | eigener Code, der entfernt wird |
|---|---|
| stabiles `SubscribeState` | schneller DSP-State-Poller |
| persistenter Runtime Source-Rate Override | Config-Rate-Patching |
| Override im Inactive-State + survives SetConfig | PreviousConfig-Rate-Rebuild |
| token-aware Source-Rate Override | `$samplerate$` Guard |
| native aloop Rate Detection | HCTL Rate Observer |
| native aloop Restart Lifecycle | Rate Reconciler |
| vollständiger Source Lifecycle | Rust-Daemon prüfen → ggf. löschen |
| Config Revision/CAS | Race-Mitigation reduzieren |
| stabile Embedded API | nur neu bewerten, nicht automatisch migrieren |

---

# 40. 4.2 / 5 Entwicklungsstrategie

Während Entwicklung:

- [ ] CamillaDSP 4.2 als Canary.
- [ ] CamillaDSP `next5` als Canary.
- [ ] derselbe Reconciler gegen beide.
- [ ] Protokolldifferenzen ausschließlich im Adapter.
- [ ] keine Userconfig-Migration in Rust.
- [ ] keine automatische Produktionsumschaltung auf Development-Branches.

Release Gate:

```text
Ist CamillaDSP 5 offiziell?
Ist CamillaGUI/pyCamillaDSP kompatibel?
Ist ARM/pCP Packaging validiert?
Sind unsere Lifecycle-Tests grün?
```

## Wenn ja

```text
v2 → CamillaDSP 5
protocol_v4.rs → DELETE
```

## Wenn nein, aber 4.2 produktionsreif

```text
v2 → CamillaDSP 4.2
protocol_v5 bleibt Canary
```

Beim späteren 5-Upgrade:

```text
v5 Adapter testen
→ Produktionsstack umstellen
→ v4 Adapter löschen
```

---

# 41. CI-Strategie

## Release-Gate CI

Nur gepinnter Produktionsstack:

- [ ] fmt.
- [ ] clippy.
- [ ] unit tests.
- [ ] integration tests.
- [ ] ARM build.
- [ ] ALSA state tests.
- [ ] Camilla protocol tests.
- [ ] Config continuity tests.
- [ ] Race tests.
- [ ] Failure recovery tests.

## Upstream Canary CI

Nicht release-blockierend:

- [ ] `next4.2.0`.
- [ ] `next5`.
- [ ] kommende GUI-Branches.
- [ ] WebSocket Contract Probe.
- [ ] Config Capability Probe.
- [ ] aloop Lifecycle Probe.
- [ ] State Event Probe.

Canary meldet:

```text
Upstream capability changed
```

aber aktualisiert kein Produkt automatisch.

---

# 42. Black-Box Capability Probes

Statt nur Source-Code-Diffs zu beobachten:

- [ ] kann CamillaDSP Loopback Rate selbst erkennen?
- [ ] kann Source Rate im Inactive-State gesetzt werden?
- [ ] persistiert Rate-Override über SetConfig?
- [ ] persistiert Rate-Override über GUI Apply?
- [ ] persistiert Rate-Override über Config-Wechsel?
- [ ] werden `$samplerate$`-Tokens korrekt neu ausgewertet?
- [ ] gibt `stop_on_inactive` Loopback zuverlässig frei?
- [ ] bleibt GetPreviousConfig nach normalem Stop korrekt?
- [ ] bleibt ConfigFilePath bei SetConfig unverändert?
- [ ] existiert Config Revision/CAS?
- [ ] funktioniert SubscribeState stabil?
- [ ] besitzt CamillaDSP einen vollständigen nativen aloop Rate Lifecycle?

Wenn Probe erstmals upstream-grün:

```text
Removal Matrix prüfen
→ Workaround löschen
```

---

# 43. Installer v2

Frisch und minimal.

- [ ] Neuinstallation only.
- [ ] `snd-aloop` prüfen.
- [ ] physisches Playback-Device einmalig erkennen.
- [ ] `pcm.picorecdsp` installieren.
- [ ] gepinntes CamillaDSP installieren.
- [ ] kompatibles gepinntes CamillaGUI installieren.
- [ ] gemeinsames natives Statefile konfigurieren.
- [ ] Bypass.yml nur bei Nichtvorhandensein erzeugen.
- [ ] Null.yml nur bei Nichtvorhandensein erzeugen.
- [ ] Rust v2 installieren, solange noch notwendig.
- [ ] Producer auf `pcm.picorecdsp` routen.
- [ ] keine Squeezelite-Parameter als Core-Voraussetzung.
- [ ] kein Backend-Menü.
- [ ] kein Backend-Switcher.
- [ ] keine Reinstall-Migration.
- [ ] keine bestehende Userconfig überschreiben.
- [ ] pCP Backup ausführen.
- [ ] Reboot falls erforderlich.

---

# 44. Bypass / Null Configs

- [ ] zur gepinnten CamillaDSP-Version passend erzeugen.
- [ ] korrektes Loopback Capture.
- [ ] S32_LE.
- [ ] 2 Channels.
- [ ] `stop_on_inactive: true`.
- [ ] erkanntes physisches Playback-Device.
- [ ] sinnvolle Rate-Adjust Defaults.
- [ ] keine piCoreCDSP-Runtime-Tokens.
- [ ] nach Installation User-owned.
- [ ] nie automatisch neu schreiben.
- [ ] bei v5 neues Schema separat validieren.
- [ ] bestehende v4-Userconfigs niemals heimlich migrieren.

---

# 45. Pflicht-Testmatrix – Source

- [ ] Boot ohne Source.
- [ ] Boot mit Source.
- [ ] Start 44.1.
- [ ] Start 48.
- [ ] Start 88.2.
- [ ] Start 96.
- [ ] Start 176.4.
- [ ] Start 192.
- [ ] Stop.
- [ ] neue Source gleiche Rate.
- [ ] neue Source andere Rate.
- [ ] rapid flapping.
- [ ] verlorenes HCTL Event.
- [ ] doppelte HCTL Events.

---

# 46. Pflicht-Testmatrix – Producer

- [ ] Squeezelite.
- [ ] AirPlay/Shairport.
- [ ] Squeezelite → AirPlay.
- [ ] AirPlay → Squeezelite.
- [ ] gleiche Rate beim Wechsel.
- [ ] andere Rate beim Wechsel.
- [ ] paralleles Open.
- [ ] Producer beendet sich unerwartet.
- [ ] Producer öffnet unmittelbar erneut.

---

# 47. Pflicht-Testmatrix – GUI

- [ ] Filter Apply.
- [ ] Mixer Apply.
- [ ] Pipeline Apply.
- [ ] Config A → B.
- [ ] Apply ohne Save.
- [ ] Save ohne Apply.
- [ ] Apply + Save.
- [ ] Apply während Source Stop.
- [ ] Apply während Source Start.
- [ ] Apply während Ratewechsel.
- [ ] Config Switch während Ratewechsel.
- [ ] Resampler aktivieren während Playback.
- [ ] Resampler deaktivieren während Playback.
- [ ] GUI Restart während Playback.

Wichtigste Regression:

```text
Disk: gain 0
Play 44.1
GUI gain +6 → Apply, NO SAVE
Rate 44.1 → 96 → 48

Expected:
gain bleibt +6
Source Rate folgt 44.1 → 96 → 48
```

- [ ] muss bestehen.

---

# 48. Failure Injection

- [ ] WebSocket disconnect.
- [ ] CamillaDSP kontrollierter Restart.
- [ ] CamillaDSP Crash.
- [ ] Rust Restart.
- [ ] Rust Crash.
- [ ] GUI Restart.
- [ ] DAC disconnect.
- [ ] DAC reconnect.
- [ ] invalid Config.
- [ ] incompatible Transport Config.
- [ ] Stalled.
- [ ] PlaybackError.
- [ ] CaptureError.
- [ ] CaptureFormatChange.
- [ ] ConfigFilePath fehlt.
- [ ] Statefile fehlt.
- [ ] Config-Datei extern verändert.
- [ ] snd-aloop fehlt.
- [ ] Loopback Handle hängt.
- [ ] WebSocket RateLimitExceeded.
- [ ] CamillaDSP noch Starting während neuem Event.

---

# 49. Hardware Gate

Vor v1→v2:

- [ ] reale pCP-Zielversion.
- [ ] reale Raspberry-Pi-Zielhardware.
- [ ] reale USB-DACs.
- [ ] I2S-DAC falls relevant.
- [ ] mehrere Producer.
- [ ] beide Ratefamilien.
- [ ] mehrere hundert Ratewechsel.
- [ ] mehrere hundert Producer-Handover.
- [ ] Long-run Playback.
- [ ] intensive GUI-Nutzung.
- [ ] Failure Injection.
- [ ] keine Userdatei verändert.
- [ ] keine Shadow-Config erzeugt.
- [ ] keine hängenden Loopback-Handles.
- [ ] keine stale Runtime-Rate.
- [ ] keine verlorenen Applied-GUI-Änderungen bei normalen Ratewechseln.
- [ ] keine unbeabsichtigten CamillaDSP-Prozessneustarts bei normalen Filteränderungen.

---

# 50. Harter v1→v2 Cleanup

Nach bestandener Hardwarevalidierung:

- [ ] `v1-final` taggen.
- [ ] optional Archivbranch.
- [ ] ioplug löschen.
- [ ] C-Code löschen.
- [ ] IPC löschen.
- [ ] stdin capture löschen.
- [ ] Ringbuffer löschen.
- [ ] Audio Workerthreads löschen.
- [ ] Backend-Abstraktion löschen.
- [ ] Backend-Switcher löschen.
- [ ] `adaptation.rs` löschen.
- [ ] RuntimeConfig löschen.
- [ ] Runtime-YAML löschen.
- [ ] Reinstall-Logik löschen.
- [ ] ioplug Benchmarks löschen.
- [ ] ioplug CI löschen.
- [ ] obsolete Upstream-Monitoring löschen.
- [ ] obsolete Docs löschen.
- [ ] README ausschließlich auf piCoreCDSP v2 ausrichten.

Kein:

```text
legacy/
deprecated/
old_controller/
experimental_ioplug/
```

Git ist das Archiv.

---

# 51. Definition of Done

piCoreCDSP v2 ist fertig, wenn:

- [ ] Architektur auf CamillaDSP 5 ausgerichtet ist.
- [ ] Produktionsstack exakt gepinnt ist.
- [ ] CamillaDSP ein separater Prozess bleibt.
- [ ] `camillalib` nicht eingebettet ist.
- [ ] genau ein produktiver Camilla-WebSocket-Adapter existiert.
- [ ] `pcm.picorecdsp` producer-agnostisch ist.
- [ ] nur `snd-aloop` verwendet wird.
- [ ] kein ioplug existiert.
- [ ] kein eigener Audio-Datapath existiert.
- [ ] Rust keine Samples verarbeitet.
- [ ] User-YAML unangetastet bleibt.
- [ ] keine Runtime-YAML existiert.
- [ ] GetConfig/GetPreviousConfig Runtime-Continuity abbilden.
- [ ] Apply ohne Save normale Ratewechsel überlebt.
- [ ] Save ohne Apply nicht automatisch applied wird.
- [ ] Config-Wechsel während Playback funktioniert.
- [ ] Source Rate ALSA folgt.
- [ ] Native-Mode funktioniert.
- [ ] Resampler-Mode funktioniert.
- [ ] GUI-/Rate-Races zuverlässig convergieren.
- [ ] Fehler nicht durch Config-Reparatur maskiert werden.
- [ ] jeder Workaround ein Removal Criterion besitzt.
- [ ] Upstream Canaries zukünftige Vereinfachungen erkennen.
- [ ] v1/ioplug vollständig aus `main` entfernt sind.

---

# 52. Langfristige Abbaustufen

## Stufe 1 – stabile State Events

```text
fast DSP polling
→ DELETE
```

## Stufe 2 – Runtime Source-Rate Override

```text
manual config rate patch
→ DELETE
```

## Stufe 3 – Override auch Inactive

```text
PreviousConfig rate rebuild
→ DELETE
```

## Stufe 4 – native Loopback Rate Detection

```text
Rust HCTL rate observer
→ DELETE
```

## Stufe 5 – nativer Loopback Lifecycle

```text
rate_sync
+ große Teile reconcile
→ DELETE
```

## Stufe 6 – vollständige Upstream-Lösung

```text
Rust daemon
→ DELETE
```

Endzustand:

```text
Producer
    ↓
pcm.picorecdsp
    ↓
snd-aloop
    ↓
CamillaDSP
    ↓
DAC
```

piCoreCDSP bleibt dann im Wesentlichen:

```text
Installer
+ ALSA Setup
+ Initialconfigs
+ Packaging
```

---

# 53. Kernphilosophie

> **piCoreCDSP v2 is not a new permanent controller platform. It is a small, replaceable compatibility bridge between ALSA source state and the CamillaDSP capabilities available today.**

> **CamillaDSP remains an independent process. piCoreCDSP integrates through public ALSA and WebSocket boundaries, never through unstable internal engine APIs.**

> **Every workaround must have a deletion path. Upstream capabilities replace local code; they do not get added beside it forever.**

Diese drei Regeln sind die Leitlinien für die gesamte Neuentwicklung.

---

# 54. Automatisches Upstream-Monitoring

Upstream-Monitoring ist Bestandteil der Architektur und nicht nur ein Wartungstool.

Ziel ist nicht lediglich:

```text
"Upstream repository changed"
```

sondern:

```text
"Eine Änderung betrifft eine Fähigkeit,
die piCoreCDSP heute selbst implementiert
oder zukünftig an Upstream abgeben könnte."
```

Das Monitoring wird deshalb **capability-aware** aufgebaut.

Grundprinzip:

```text
Upstream source change
        ↓
relevante Dateien spiegeln
        ↓
betroffene Capability bestimmen
        ↓
statische Contract Checks
        ↓
Black-Box Capability Probes
        ↓
Status/Report/PR
        ↓
Removal Matrix prüfen
```

---

# 55. Upstream-Quellen – Priorität A: direkt produktionskritisch

## 55.1 CamillaDSP Engine

Repository:

```text
HEnquist/camilladsp
```

Zu beobachten:

```text
master
next4.2.0
next5
Releases
offene PRs gegen master
```

Für piCoreCDSP relevante Bereiche:

```text
src/alsa_backend/
src/config/
src/engine.rs
src/websocket_server/
src/statefile.rs
src/bin.rs
backend_alsa.md
websocket.md
CHANGELOG.md
Cargo.toml
README.crates.md
```

Capabilities:

- `camilla.websocket.protocol`
- `camilla.state.events`
- `camilla.runtime.active_config`
- `camilla.runtime.previous_config`
- `camilla.runtime.config_path`
- `camilla.config.set`
- `camilla.config.set_value`
- `camilla.source_rate.override`
- `camilla.alsa.loopback.active`
- `camilla.alsa.loopback.rate`
- `camilla.alsa.loopback.lifecycle`
- `camilla.stop_on_inactive`
- `camilla.config.revision`
- `camilla.token.samplerate`
- `camilla.statefile`
- `camilla.process.lifecycle`

Aktuell von piCoreCDSP direkt genutzt:

- [ ] WebSocket Control API.
- [ ] Processing State.
- [ ] StopReason.
- [ ] `GetConfig`.
- [ ] `GetPreviousConfig`.
- [ ] `GetConfigFilePath`.
- [ ] `SetConfig`.
- [ ] `SetConfigValue`.
- [ ] `stop_on_inactive`.
- [ ] Statefile.

Zukünftig besonders relevant:

- [ ] Runtime Source-Rate Override.
- [ ] Source-Rate Override im Inactive-State.
- [ ] native `snd-aloop` Rate Detection.
- [ ] nativer Loopback Restart Lifecycle.
- [ ] Config Revision/CAS.
- [ ] token-aware Runtime Overrides.
- [ ] weitere State Push Events.

---

## 55.2 CamillaGUI Backend

Repository:

```text
HEnquist/camillagui-backend
```

Zu beobachten:

```text
master
next4.2.0
Releases
zukünftige next5/v5 Branches
```

Relevante Bereiche:

```text
backend/
config/
release_automation/
main.py
README.md
```

Besonders:

```text
backend/filemanagement.py
backend/settings.py
backend/settings_schemas.py
release_automation/versions.yml
```

Capabilities:

- `gui.active_config.path`
- `gui.statefile.integration`
- `gui.apply`
- `gui.save`
- `gui.eventstream`
- `gui.camilla.version_compat`
- `gui.config.path_resolution`
- `gui.runtime_vs_saved_config`

Direkt relevant:

- [ ] native Statefile-Integration.
- [ ] Active-Config-Verhalten.
- [ ] Apply-/Save-Semantik.
- [ ] ConfigFilePath-Verhalten.
- [ ] CamillaDSP-/pyCamillaDSP-Kompatibilitätsversionen.

Zukünftig relevant:

- [ ] Runtime-vs-Persistent-Config-Anzeige.
- [ ] Runtime Override Awareness.
- [ ] native State Event Integration.
- [ ] CamillaDSP-5-Kompatibilität.

---

## 55.3 CamillaGUI Frontend

Repository:

```text
HEnquist/camillagui
```

Zu beobachten:

```text
master
next4.2.0
Releases
zukünftige next5/v5 Branches
```

Priorität niedriger als `camillagui-backend`, aber wichtig für:

- [ ] Apply-Workflow.
- [ ] Config-Switch-Workflow.
- [ ] Anzeige von Runtime-State.
- [ ] neue UI-Semantik bei Runtime Overrides.
- [ ] Breaking UI/API-Annahmen zwischen GUI und Backend.

Nicht vollständig spiegeln.

Nur Metadaten, Releaseinformationen und gezielt relevante UI-/API-Dateien.

---

## 55.4 pyCamillaDSP

Repository:

```text
HEnquist/pycamilladsp
```

Zu beobachten:

```text
master
next4.1.0
zukünftige 4.2/5 Branches
Releases
```

Warum wichtig:

CamillaGUI verwendet den Python-Client als Teil seines CamillaDSP-Stacks.
Neue CamillaDSP-WebSocket-Funktionen werden häufig hier als Client-API sichtbar,
bevor oder während die GUI sie übernimmt.

Relevante Bereiche:

```text
camilladsp/
docs/
tests/test_camillaws.py
```

Capabilities:

- `pycamilla.protocol.version`
- `pycamilla.state.events`
- `pycamilla.config.active`
- `pycamilla.config.previous`
- `pycamilla.config.path`
- `pycamilla.set_config`
- `pycamilla.set_config_value`
- `pycamilla.error_semantics`

Monitoring-Fragen:

- [ ] gibt es einen v5-kompatiblen Branch?
- [ ] wird das neue v5-WebSocket-Protokoll unterstützt?
- [ ] wird `SubscribeState` unterstützt?
- [ ] ändern sich Config-/Error-Semantiken?
- [ ] welche CamillaDSP-Versionen werden offiziell unterstützt?

---

# 56. Upstream-Quellen – Priorität B: Referenz und zukünftige Removal-Signale

## 56.1 Offizieller CamillaDSP Controller

Repository:

```text
HEnquist/camilladsp-controller
```

Zu beobachten:

```text
main
Releases/Tags falls vorhanden
```

Relevante Dateien:

```text
alsa_listener.py
controller.py
config_provider.py
```

Nutzen:

Dieser Controller ist kein Produktionsdependency von piCoreCDSP.

Er dient als **Upstream-Referenzimplementation** für:

- [ ] ALSA HCTL Monitoring.
- [ ] Debounce-Verhalten.
- [ ] `PCM Slave Active`.
- [ ] `PCM Slave Rate`.
- [ ] Format/Channels Snapshot.
- [ ] Source-Transition-Semantik.
- [ ] CamillaDSP Runtime-Coordination.

Capabilities:

- `reference.alsa_listener`
- `reference.aloop.snapshot`
- `reference.debounce`
- `reference.rate_switch`
- `reference.config_provider`

Wenn Upstream hier neue robuste Loopback-Strategien einführt:

```text
Review required
→ mit source/alsa_loopback.rs vergleichen
```

Nicht automatisch Code übernehmen.

---

## 56.2 ALSA userspace

Repository:

```text
alsa-project/alsa-lib
```

Branch:

```text
master
```

Relevante Bereiche:

```text
src/pcm/pcm_plug.c
src/pcm/
src/control/
include/
doc/asoundrc.txt
```

Capabilities:

- `alsa.plug.format`
- `alsa.plug.channels`
- `alsa.plug.rate_unchanged`
- `alsa.plug.route_policy`
- `alsa.hctl`
- `alsa.control.events`

Direkt genutzt:

- [ ] `type plug`.
- [ ] Format-Normalisierung.
- [ ] Channel-Normalisierung.
- [ ] `rate unchanged`.
- [ ] HCTL/Control API.

Monitoring:

- [ ] Änderung der `plug`-Negotiation.
- [ ] Änderung von `rate unchanged`.
- [ ] Änderung von Channel-Routing.
- [ ] Änderungen der HCTL/Event-API.
- [ ] relevante ABI-/API-Änderungen.

---

## 56.3 Linux `snd-aloop` – Canonical Upstream

Repository:

```text
torvalds/linux
```

Relevante Datei:

```text
sound/drivers/aloop.c
```

Optional zusätzlich relevante Kernel-Doku:

```text
Documentation/sound/
```

Capabilities:

- `kernel.aloop.active`
- `kernel.aloop.rate`
- `kernel.aloop.format`
- `kernel.aloop.channels`
- `kernel.aloop.pcm_notify`
- `kernel.aloop.release_semantics`

Besonders beobachten:

- [ ] `PCM Slave Active`.
- [ ] `PCM Slave Rate`.
- [ ] `PCM Slave Format`.
- [ ] `PCM Slave Channels`.
- [ ] `pcm_notify`.
- [ ] `snd_ctl_notify`.
- [ ] Close/Open- und Formatwechsel-Semantik.

Diese Quelle sagt:

```text
Was Linux upstream grundsätzlich kann.
```

Sie sagt nicht automatisch:

```text
Was auf piCorePlayer bereits verfügbar ist.
```

---

# 57. Upstream-Quellen – Priorität A für die reale Zielplattform

## 57.1 piCorePlayer Linux Kernel

Repository:

```text
piCorePlayer/linux
```

Wichtig:

Dies ist für Produktionsentscheidungen wichtiger als ausschließlich `torvalds/linux`,
weil hier der tatsächlich von pCP eingesetzte bzw. gepatchte Kernelstand sichtbar wird.

Relevante Datei:

```text
sound/drivers/aloop.c
```

Capabilities:

- `pcp.kernel.aloop.active`
- `pcp.kernel.aloop.rate`
- `pcp.kernel.aloop.pcm_notify`
- `pcp.kernel.version`

Prüfen:

- [ ] entspricht `snd-aloop` dem Canonical Upstream?
- [ ] fehlen neue relevante aloop-Patches?
- [ ] gibt es pCP-spezifische Änderungen?
- [ ] welche Kernel-Version ist aktuelles Produktionsziel?

---

## 57.2 piCorePlayer Kernel Config / Symbols

Repository:

```text
piCorePlayer/pCP-Kernels
```

Relevant für:

- [ ] `CONFIG_SND_ALOOP`.
- [ ] Modulverfügbarkeit.
- [ ] Architektur armv7/aarch64.
- [ ] Kernel-ABI.
- [ ] Packaging von Kernelmodulen.

Capability:

```text
pcp.snd_aloop.available
```

Installer-Gate:

```text
snd-aloop unavailable
→ Installation abbrechen
```

---

## 57.3 piCorePlayer Releases

Repository:

```text
piCorePlayer/pCP-Releases
```

Relevant für:

- [ ] neue pCP-Versionen.
- [ ] Image-/Architekturänderungen.
- [ ] Kernelwechsel.
- [ ] mögliche Änderung der unterstützten Plattformen.

Zusätzlich kann die offizielle pCP-Dokumentation als externe Releasequelle
beobachtet werden, aber der GitHub-Workflow soll primär auf GitHub-Quellen beruhen.

---

# 58. Quellen, die wir nicht vollständig spiegeln sollten

Nicht alles gehört in den Mirror.

Nicht nötig als Full Mirror:

- `torvalds/linux` komplett.
- `piCorePlayer/linux` komplett.
- komplettes `camillagui` Frontend.
- `pycamilladsp-plot`.
- CamillaDSP Benchmarks außerhalb unserer Capabilities.
- allgemeine DSP-Filterimplementationen ohne Bezug zu unserem Contract.

Stattdessen:

> **Sparse upstream snapshot + immutable source metadata**

Jeder Snapshot enthält:

```text
repository
ref
commit_sha
fetched_at
relevant_paths
release/tag metadata
```

---

# 59. Vorgeschlagene Repository-Struktur für den Upstream Mirror

```text
upstream/
├── manifest.yml
├── status.json
├── capabilities.yml
│
├── camilladsp/
│   ├── master/
│   ├── next4.2.0/
│   └── next5/
│
├── camillagui-backend/
│   ├── master/
│   └── next4.2.0/
│
├── camillagui/
│   ├── master/
│   └── next4.2.0/
│
├── pycamilladsp/
│   └── master/
│
├── camilladsp-controller/
│   └── main/
│
├── alsa-lib/
│   └── master/
│
├── linux-aloop/
│   └── master/
│
├── pcp-linux-aloop/
│   └── current/
│
├── pcp-kernels/
│   └── current/
│
└── pcp-releases/
    └── current/
```

Nur relevante Dateien werden gespeichert.

---

# 60. `upstream/manifest.yml`

Der Mirror wird deklarativ gesteuert.

Beispiel:

```yaml
version: 1

sources:
  - id: camilladsp-next5
    repo: HEnquist/camilladsp
    ref: next5
    priority: critical
    paths:
      - src/alsa_backend/**
      - src/config/**
      - src/engine.rs
      - src/websocket_server/**
      - src/statefile.rs
      - src/bin.rs
      - backend_alsa.md
      - websocket.md
      - CHANGELOG.md
      - Cargo.toml
    capabilities:
      - camilla.websocket.protocol
      - camilla.state.events
      - camilla.source_rate.override
      - camilla.alsa.loopback.rate
      - camilla.alsa.loopback.lifecycle
      - camilla.config.revision
      - camilla.token.samplerate

  - id: alsa-lib-master
    repo: alsa-project/alsa-lib
    ref: master
    priority: high
    paths:
      - src/pcm/pcm_plug.c
      - src/control/**
      - doc/asoundrc.txt
    capabilities:
      - alsa.plug.rate_unchanged
      - alsa.plug.channels
      - alsa.plug.route_policy
      - alsa.hctl

  - id: linux-aloop
    repo: torvalds/linux
    ref: master
    priority: high
    paths:
      - sound/drivers/aloop.c
    capabilities:
      - kernel.aloop.active
      - kernel.aloop.rate
      - kernel.aloop.pcm_notify

  - id: pcp-linux-aloop
    repo: piCorePlayer/linux
    ref: auto
    priority: critical
    paths:
      - sound/drivers/aloop.c
    capabilities:
      - pcp.kernel.aloop.rate
      - pcp.kernel.aloop.pcm_notify
```

`ref: auto` bedeutet:

```text
aktuellen Default-/Produktionsbranch des pCP-Repos auflösen
und SHA explizit im Status speichern.
```

---

# 61. `upstream/capabilities.yml`

Jede Capability wird mit unserem Code verknüpft.

Beispiel:

```yaml
capabilities:

  camilla.state.events:
    used_now: true
    local_code:
      - src/camilla/
      - src/reconcile.rs
    current_fallback:
      - dsp_state_polling
    removal_when:
      - stable_subscribe_state

  camilla.source_rate.override:
    used_now: false
    wanted_future: true
    local_code:
      - src/rate_sync/config_patch.rs
    current_fallback:
      - set_config_value
      - previous_config_rebuild
    removal_when:
      - runtime_override_works_while_inactive
      - override_survives_set_config
      - override_survives_gui_apply
      - override_is_token_aware

  camilla.alsa.loopback.lifecycle:
    used_now: false
    wanted_future: true
    local_code:
      - src/source/alsa_loopback.rs
      - src/rate_sync/
      - src/reconcile.rs
    removal_when:
      - camilla_detects_loopback_rate
      - camilla_restarts_on_new_rate
      - camilla_releases_on_inactive

  alsa.plug.rate_unchanged:
    used_now: true
    local_code:
      - installer/
      - assets/asound.conf
    required_contract:
      - source_rate_is_not_resampled

  kernel.aloop.pcm_notify:
    used_now: false
    wanted_future: true
    local_code:
      - src/source/alsa_loopback.rs
    note:
      - architecture_must_not_depend_on_this_until_hardware_validated
```

Damit kann ein Workflow automatisch ausgeben:

```text
Changed upstream file:
src/alsa_backend/utils.rs

Affected capabilities:
- camilla.alsa.loopback.rate
- camilla.alsa.loopback.lifecycle

Potential local code:
- src/source/alsa_loopback.rs
- src/rate_sync/
- src/reconcile.rs

Removal candidate:
YES
```

---

# 62. GitHub Workflow 1 – `upstream-sync.yml`

Schedule:

```text
täglich
+
workflow_dispatch
```

Aufgaben:

- [ ] `upstream/manifest.yml` lesen.
- [ ] aktuellen SHA jedes Sources auflösen.
- [ ] nur relevante Pfade abrufen.
- [ ] Snapshot aktualisieren.
- [ ] Release-/Branch-Metadaten erfassen.
- [ ] `upstream/status.json` aktualisieren.
- [ ] Diff nach Capabilities klassifizieren.
- [ ] keine direkten Pushes auf `main`.
- [ ] automatischen Branch erzeugen.
- [ ] automatischen PR öffnen.

PR-Titel beispielsweise:

```text
chore(upstream): sync CamillaDSP next5 @ 8ed2e53
```

PR-Body:

```text
Sources changed:
- camilladsp-next5

Capabilities touched:
- camilla.websocket.protocol
- camilla.alsa.loopback.rate

Local areas potentially affected:
- src/camilla/protocol_v5.rs
- src/source/alsa_loopback.rs

Capability probes:
- websocket_contract: PASS
- aloop_native_rate_follow: FAIL
- runtime_rate_override: FAIL

Removal candidates:
- none
```

---

# 63. GitHub Workflow 2 – `upstream-capability-canary.yml`

Trigger:

```text
nach Upstream-Sync-PR
+
nightly
+
workflow_dispatch
```

Führt keine reine Source-Analyse aus, sondern Black-Box-Probes.

Probe-Gruppen:

## CamillaDSP

- [ ] WebSocket Protocol Contract.
- [ ] `GetConfig`.
- [ ] `GetPreviousConfig`.
- [ ] ConfigFilePath.
- [ ] `SetConfig`.
- [ ] `SetConfigValue`.
- [ ] State Events.
- [ ] `stop_on_inactive`.
- [ ] Source Rate Override vorhanden?
- [ ] Override while Inactive?
- [ ] Override survives SetConfig?
- [ ] Override survives GUI Apply?
- [ ] Config Revision/CAS?
- [ ] `$samplerate$` token-aware override?

## ALSA / Kernel

Wo CI-Umgebung dies unterstützt:

- [ ] `snd-aloop` laden.
- [ ] HCTL controls vorhanden.
- [ ] Active Transition.
- [ ] Rate Snapshot.
- [ ] Format Snapshot.
- [ ] Channel Snapshot.
- [ ] Close/Open Ratewechsel.
- [ ] `pcm_notify` Capability.

Hardwareabhängige Probes werden zusätzlich auf realem pCP-Testgerät ausgeführt und nicht durch GitHub-hosted CI simuliert.

---

# 64. GitHub Workflow 3 – `upstream-release-watch.yml`

Schedule:

```text
täglich
```

Beobachtet:

- CamillaDSP Releases.
- CamillaGUI Backend Releases.
- CamillaGUI Releases.
- pyCamillaDSP Releases.
- piCorePlayer Releases.

Meldet:

```text
new release available
```

mit:

```text
currently pinned
new version
breaking/not breaking
canary status
production eligibility
```

Automatisches Upgrade:

```text
NEIN
```

Stattdessen:

```text
Release discovered
→ Canary
→ Hardware validation
→ bewusste Produktentscheidung
```

---

# 65. GitHub Workflow 4 – `upstream-branch-watch.yml`

Speziell wichtig während der 4.2/5-Transition.

Überwacht neu auftauchende Branches:

```text
next*
v5*
5.*
```

Besonders bei:

```text
HEnquist/camillagui-backend
HEnquist/camillagui
HEnquist/pycamilladsp
```

Beispiel:

```text
pycamilladsp erhält next5
```

→ automatisch Issue/Report:

```text
CamillaDSP 5 ecosystem signal detected:
pyCamillaDSP next5 branch appeared.
```

Dies ist ein starkes Release-Reife-Signal.

---

# 66. GitHub Workflow 5 – `upstream-removal-check.yml`

Trigger:

```text
wenn ein Capability Probe von FAIL → PASS wechselt
```

Beispiel:

```text
camilla.source_rate.override:
FAIL → PASS
```

Workflow erzeugt kein automatisches Code-Delete.

Stattdessen öffnet er ein Issue:

```text
Removal candidate: config_patch rate synchronizer
```

Inhalt:

```text
Upstream capability:
camilla.source_rate.override

Previously:
FAIL

Now:
PASS

Potentially removable local code:
- src/rate_sync/config_patch.rs

Required validation before removal:
- inactive state
- GUI Apply
- Config switch
- $samplerate$
- resampler mode
- hardware 44.1/48/96/192
```

Nach erfolgreicher Hardwarevalidierung:

```text
Workaround entfernen.
```

---

# 67. Monitoring-Level

Nicht jede Änderung ist gleich wichtig.

## Critical

Sofort Canary + Review:

- CamillaDSP WebSocket.
- CamillaDSP Config Lifecycle.
- CamillaDSP ALSA Loopback.
- `snd-aloop`.
- pCP Kernel / snd-aloop.
- Statefile.
- GetConfig/GetPreviousConfig.
- SetConfig/SetConfigValue.

## High

Automatischer PR + Tests:

- CamillaGUI Backend.
- pyCamillaDSP.
- alsa-lib plug/HCTL.
- CamillaDSP Controller ALSA listener.
- pCP kernel config.

## Medium

Report:

- CamillaGUI Frontend.
- Docs.
- packaging-related upstream changes.

## Ignore unless dependency changes

- allgemeine CamillaDSP Filter.
- unrelated backends.
- benchmark-only changes.
- Windows/macOS-only code.
- unrelated GUI widgets.

---

# 68. Upstream Status Dashboard

Automatisch erzeugte Datei:

```text
upstream/status.md
```

Beispiel:

```text
CamillaDSP
  production: 4.x pinned
  next4.2: SHA ...
  next5: SHA ...
  websocket-v5: PASS
  subscribe-state: PASS
  native-aloop-rate: FAIL
  runtime-source-rate-override: FAIL

CamillaGUI
  backend-v5-compatible: FAIL
  frontend-v5-compatible: UNKNOWN

pyCamillaDSP
  v5-client: FAIL

ALSA
  plug-rate-unchanged: PASS
  hctl-controls: PASS

Kernel upstream
  pcm_notify: AVAILABLE

pCP target kernel
  pcm_notify: AVAILABLE/UNKNOWN
```

Damit ist auf einen Blick sichtbar:

```text
Was verwenden wir?
Was kann upstream?
Was fehlt noch?
Welcher lokale Code ist dadurch nötig?
```

---

# 69. Keine automatische Upstream-Codeübernahme

Mirror bedeutet ausdrücklich nicht:

```text
git subtree merge upstream code into production
```

Regeln:

- [ ] Upstream-Snapshots sind Read-only Referenz.
- [ ] Keine automatische Übernahme von Source-Patches.
- [ ] Keine automatische Configmigration.
- [ ] Kein automatisches Versionsupgrade.
- [ ] Kein automatisches Löschen lokaler Workarounds.
- [ ] Alle Produktionsänderungen laufen über normale Review-/Hardware-Gates.

Der Mirror dient:

```text
Detection
Analysis
Capability verification
Removal planning
```

nicht:

```text
automatic integration
```

---

# 70. Retention Policy für den Mirror

Nicht jeden historischen Snapshot dauerhaft speichern.

Im Git-Repository behalten:

- [ ] aktuellen Snapshot.
- [ ] vorherigen Snapshot für Diff.
- [ ] SHA-/Release-Historie in `status.json`.
- [ ] wichtige Capability-Transitions.

Nicht behalten:

- [ ] vollständige Linux-Historie.
- [ ] vollständige Kopie externer Repositories.
- [ ] Binärartefakte.
- [ ] große Buildoutputs.

GitHub Actions Artifacts dürfen umfangreichere temporäre Testdaten enthalten.

---

# 71. Automatisches Issue-Labeling

Empfohlene Labels:

```text
upstream
upstream:camilladsp
upstream:camillagui
upstream:alsa
upstream:kernel
upstream:pcp

capability
removal-candidate
breaking-change
canary-failure
release-candidate
```

Beispiel:

```text
[upstream][removal-candidate]
CamillaDSP next5 now supports runtime source-rate override
```

---

# 72. Upstream-Monitoring Definition of Done

- [ ] `upstream/manifest.yml` existiert.
- [ ] alle Sources sind einer Priorität zugeordnet.
- [ ] alle relevanten Sourcepfade sind definiert.
- [ ] jede Source ist mindestens einer Capability zugeordnet.
- [ ] jede eigene Compatibility-Bridge verweist auf eine Capability.
- [ ] jede Capability kennt den lokalen Code, den sie später ersetzen kann.
- [ ] täglicher Upstream-Sync läuft.
- [ ] Sync erzeugt PR statt direktem Main-Push.
- [ ] Capability-Canary läuft automatisiert.
- [ ] Release-Watch läuft automatisiert.
- [ ] Branch-Watch erkennt neue 4.2/5-Ökosystemzweige.
- [ ] FAIL→PASS erzeugt Removal-Candidate-Issue.
- [ ] Produktionsupgrade bleibt manuell.
- [ ] Hardwareabhängige Capabilities besitzen separates pCP-Hardware-Gate.
- [ ] Upstream-Statusdashboard wird automatisch erzeugt.

---

# 73. Erweiterte Kernphilosophie

> **piCoreCDSP überwacht Upstream nicht nur auf neue Versionen, sondern auf neue Fähigkeiten.**

> **Jede heute lokale Funktion wird mit der Upstream-Capability verknüpft, die sie eines Tages ersetzen soll.**

> **Ein Upstream-Update ist erst dann für piCoreCDSP interessant, wenn ein Capability-Probe zeigt, dass sich unser konkreter Systemvertrag geändert hat.**

> **Der beste Upstream-Erfolg ist nicht neuer Code in piCoreCDSP, sondern Code, den wir aus piCoreCDSP löschen können.**

