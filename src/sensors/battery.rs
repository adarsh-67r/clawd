// battery via org.freedesktop.UPower (system bus, blocking zbus).
// The pet mostly cares about two fields: percent + charging (reactions in
// later integration decides the low-battery pose from these).

use zbus::blocking::Proxy;

#[derive(Debug, PartialEq)]
pub struct Battery {
    pub charging: bool,
    pub percent: u8,
}

/// UPower state enum: 0 unknown, 1 charging, 2 discharging, 3 empty,
/// 4 fully-charged, 5 pending-charge, 6 pending-discharge.
pub fn parse(percentage: Option<f64>, state: Option<u32>) -> Option<Battery> {
    let percent = percentage?;
    Some(Battery {
        charging: matches!(state, Some(1 | 4 | 5)),
        percent: percent.clamp(0.0, 100.0).round() as u8,
    })
}

/// Reads the DisplayDevice aggregate (the AC + battery rollup UPower maintains).
/// None = no UPower / no battery — callers fall back to the desktop-charging look.
pub fn read() -> Option<Battery> {
    let conn = zbus::blocking::Connection::system().ok()?;
    let proxy = Proxy::new(
        &conn,
        "org.freedesktop.UPower",
        "/org/freedesktop/UPower/devices/DisplayDevice",
        "org.freedesktop.UPower.Device",
    )
    .ok()?;
    let percentage: Option<f64> = proxy.get_property("Percentage").ok();
    let state: Option<u32> = proxy.get_property("State").ok();
    parse(percentage, state)
}

#[cfg(test)]
mod tests {
    use super::parse;
    use super::Battery;

    fn b(charging: bool, percent: u8) -> Option<Battery> {
        Some(Battery { charging, percent })
    }

    #[test]
    fn charging_full_reads_true() {
        assert_eq!(parse(Some(87.0), Some(1)), b(true, 87));
    }

    #[test]
    fn fully_charged_state_counts_as_charging() {
        assert_eq!(parse(Some(100.0), Some(4)), b(true, 100));
    }

    #[test]
    fn discharging_reads_false() {
        assert_eq!(parse(Some(15.6), Some(2)), b(false, 16));
    }

    #[test]
    fn missing_percentage_is_none() {
        assert_eq!(parse(None, Some(2)), None);
    }

    #[test]
    fn missing_state_defaults_not_charging() {
        assert_eq!(parse(Some(50.0), None), b(false, 50));
    }

    #[test]
    fn weird_values_saturate() {
        assert_eq!(parse(Some(120.0), Some(1)), b(true, 100));
        assert_eq!(parse(Some(-3.0), Some(2)), b(false, 0));
    }
}
