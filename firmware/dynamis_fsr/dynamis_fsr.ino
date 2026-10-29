// Dynamis firmware for the Arduino Uno R3 (ATmega328P) and Uno R4 WiFi (Renesas RA4M1).
// The Dynamis app installs this automatically; you never need the Arduino IDE.
// To change it: edit, then run scripts/build_firmware.ps1 and rebuild the app
// (bump FW_VERSION here and in src-tauri/src/device.rs so boards get updated).
//
// Sensor: TE FX29 I2C load cell at address 0x28, on the R4 WiFi's Qwiic port or SDA/SCL.
//   Wires: red V+ (3.3 V on Qwiic, or 5 V), black GND, white SDA, yellow SCL.
//   Found automatically, and looked for again every 0.5 s while it's missing.
//
// Output, 115200 baud:
//   "DYNAMIS FSR <version>" at start-up and in reply to "?"
//   "F <counts>" every 20 ms; 1000 counts = no force, 15000 = full scale (datasheet)
//   "F -" every 0.5 s while no FX29 is found

#include <Wire.h>

#define FW_ID "DYNAMIS FSR 4"

const unsigned long PERIOD_MS = 20;    // 50 readings per second
const unsigned long PROBE_MS = 500;    // how often to look for the FX29 while it's missing
const uint8_t FX29_ADDR = 0x28;
const unsigned long FX29_CONVERT_MS = 10;  // sleep-mode conversion takes 8.4 ms

unsigned long lastSend = 0;
TwoWire *fx29 = nullptr;  // the bus the FX29 is on, or null while it's missing
int fx29Failures = 0;

static bool fx29Present(TwoWire &bus) {
  bus.beginTransmission(FX29_ADDR);
  return bus.endTransmission() == 0;
}

static void findFx29() {
  fx29Failures = 0;
#if defined(ARDUINO_UNOR4_WIFI)
  if (fx29Present(Wire1)) { fx29 = &Wire1; return; }  // Qwiic port
#endif
  if (fx29Present(Wire)) { fx29 = &Wire; return; }    // SDA/SCL pins
  fx29 = nullptr;
}

// Measurement request, wait for the conversion, then fetch 2 bytes:
// bits 15-14 status, bits 13-0 the force count. Returns -1 on failure.
// The request is a read (Read_MR in the datasheet). After power-up the FX29 ignores an
// empty write as a request and keeps reporting 16383 until it has seen a read.
// Depending on timing the fetched count can be marked stale (status 2, already read); it is
// still a real measurement at most one period old, so only command mode (1) and
// diagnostic (3) are rejected.
static long fx29Read() {
  if (fx29->requestFrom(FX29_ADDR, (uint8_t)1) != 1) return -1;
  while (fx29->available()) fx29->read();
  delay(FX29_CONVERT_MS);
  if (fx29->requestFrom(FX29_ADDR, (uint8_t)2) != 2) return -1;
  uint8_t hi = fx29->read();
  uint8_t lo = fx29->read();
  uint8_t status = hi >> 6;
  if (status == 1 || status == 3) return -1;  // command mode or diagnostic
  return ((long)(hi & 0x3F) << 8) | lo;
}

void setup() {
  Serial.begin(115200);
  Wire.begin();
  Wire.setClock(100000);
#if defined(ARDUINO_ARCH_AVR)
  Wire.setWireTimeout(3000, true);  // don't freeze if a loose wire disturbs the bus
#endif
#if defined(ARDUINO_UNOR4_WIFI)
  Wire1.begin();
  Wire1.setClock(100000);
#endif
  findFx29();
  Serial.println(F(FW_ID));
}

void loop() {
  while (Serial.available()) {
    if (Serial.read() == '?') Serial.println(F(FW_ID));
  }

  if (!fx29) {
    if (millis() - lastSend < PROBE_MS) return;
    lastSend = millis();
    findFx29();
    if (!fx29) Serial.println(F("F -"));
    return;
  }

  if (millis() - lastSend < PERIOD_MS) return;
  lastSend = millis();
  long counts = fx29Read();
  if (counts >= 0) {
    fx29Failures = 0;
    Serial.print(F("F "));
    Serial.println(counts);
  } else if (++fx29Failures >= 10) {
    fx29 = nullptr;  // unplugged: report it missing until it's found again
  }
}
