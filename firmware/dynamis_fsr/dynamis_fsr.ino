// Dynamis firmware for the Arduino Uno R3 (ATmega328P) and Uno R4 WiFi (Renesas RA4M1).
// The Dynamis app installs this automatically; you never need the Arduino IDE.
// To change it: edit, then run scripts/build_firmware.ps1 and rebuild the app
// (bump FW_VERSION here and in src-tauri/src/device.rs so boards get updated).
//
// Sensors (found automatically, checked again every 2 s so they can be swapped while running):
//   TE FX29 I2C load cell at address 0x28: on the R4 WiFi's Qwiic port, or SDA/SCL.
//     Wires: red V+ (3.3 V on Qwiic, or 5 V), black GND, white SDA, yellow SCL.
//   Otherwise a thin-film force sensor (FSR) between A0 and GND, either way round.
//     A0's internal pull-up completes the divider, so no resistor is needed.
//
// Output, 115200 baud:
//   "DYNAMIS FSR <version>" at start-up and in reply to "?"
//   FX29:  "F <counts>" every 20 ms; 1000 counts = no force, 15000 = full scale (datasheet)
//   FSR:   "<reading>"  every 20 ms; 0.0 (no force) .. 1023.0 (sensor shorted)

#include <Wire.h>

#define FW_ID "DYNAMIS FSR 2"

const int SENSOR_PIN = A0;
const int SAMPLES = 16;                // FSR readings averaged per output line
const unsigned long PERIOD_MS = 20;    // 50 lines per second
const unsigned long PROBE_MS = 2000;   // how often to look for an FX29 when none is found
const uint8_t FX29_ADDR = 0x28;
const unsigned long FX29_CONVERT_MS = 10;  // sleep-mode conversion takes 8.4 ms

unsigned long lastSend = 0;
unsigned long lastProbe = 0;
TwoWire *fx29 = nullptr;  // the bus the FX29 is on, or null when using the FSR
int fx29Failures = 0;

// ---------- FX29 ----------

static bool fx29Present(TwoWire &bus) {
  bus.beginTransmission(FX29_ADDR);
  return bus.endTransmission() == 0;
}

static void findFx29() {
#if defined(ARDUINO_UNOR4_WIFI)
  if (fx29Present(Wire1)) { fx29 = &Wire1; fx29Failures = 0; return; }  // Qwiic port
#endif
  if (fx29Present(Wire)) { fx29 = &Wire; fx29Failures = 0; return; }    // SDA/SCL pins
  fx29 = nullptr;
}

// Measurement request, wait for the conversion, then fetch 2 bytes:
// bits 15-14 status (0 = fresh data), bits 13-0 the force count. Returns -1 on failure.
static long fx29Read() {
  fx29->beginTransmission(FX29_ADDR);
  if (fx29->endTransmission() != 0) return -1;
  delay(FX29_CONVERT_MS);
  if (fx29->requestFrom(FX29_ADDR, (uint8_t)2) != 2) return -1;
  uint8_t hi = fx29->read();
  uint8_t lo = fx29->read();
  if ((hi >> 6) != 0) return -1;  // stale or diagnostic
  return ((long)(hi & 0x3F) << 8) | lo;
}

// ---------- film sensor ----------

#if defined(ARDUINO_ARCH_RENESAS)
// On the R4, analogRead() reconfigures the pin on every call, which turns its pull-up off.
// So set the ADC up once with analogRead(), switch the pull-up on while the pin stays in
// analog mode, and from then on start conversions directly on the ADC.
// A0 is port P014 = ADC channel AN009.
static const int A0_PORT = 0, A0_PIN = 14, A0_CHANNEL = 9;
static int adcShift = 4;  // raw result bits beyond 10

static void sensorBegin() {
  analogRead(SENSOR_PIN);
  R_BSP_PinAccessEnable();
  R_PFS->PORT[A0_PORT].PIN[A0_PIN].PmnPFS_b.PCR = 1;
  R_BSP_PinAccessDisable();
  uint8_t adprc = R_ADC0->ADCER_b.ADPRC;  // 0 = 12-bit, 1 = 10-bit, 3 = 14-bit
  adcShift = adprc == 1 ? 0 : adprc == 0 ? 2 : 4;
}

static int sensorRead() {
  R_ADC0->ADCSR_b.ADST = 1;
  while (R_ADC0->ADCSR_b.ADST) {}
  return R_ADC0->ADDR[A0_CHANNEL] >> adcShift;
}
#else
static void sensorBegin() { pinMode(SENSOR_PIN, INPUT_PULLUP); }
static int sensorRead() { return analogRead(SENSOR_PIN); }
#endif

// ---------- main ----------

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
  sensorBegin();
  findFx29();
  Serial.println(F(FW_ID));
}

void loop() {
  while (Serial.available()) {
    if (Serial.read() == '?') Serial.println(F(FW_ID));
  }

  if (millis() - lastSend < PERIOD_MS) return;
  lastSend = millis();

  if (!fx29 && millis() - lastProbe >= PROBE_MS) {
    lastProbe = millis();
    findFx29();
  }

  if (fx29) {
    long counts = fx29Read();
    if (counts >= 0) {
      fx29Failures = 0;
      Serial.print(F("F "));
      Serial.println(counts);
    } else if (++fx29Failures >= 10) {
      fx29 = nullptr;  // unplugged: fall back to the film sensor until it's found again
      lastProbe = millis();
    }
    return;
  }

  long sum = 0;
  for (int i = 0; i < SAMPLES; i++) {
    sum += sensorRead();
  }
  // Pressing lowers the FSR's resistance and pulls A0 toward GND; invert so force reads upward.
  Serial.println(1023.0 - (float)sum / SAMPLES, 1);
}
