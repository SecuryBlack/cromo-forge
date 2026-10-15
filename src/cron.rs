//! Expresiones cron de 5 campos (minuto hora día-mes mes día-semana), en UTC.
//!
//! Admite `*`, listas (`1,5`), rangos (`1-5`) y pasos (`*/15`, `1-30/5`,
//! `5/10` = desde 5 hasta el máximo). Sin nombres (`MON`, `JAN`) ni macros
//! (`@daily`). Día de la semana 0-7, con 0 y 7 domingo. Como en el cron
//! estándar, si día del mes y día de la semana están restringidos los dos
//! (ninguno empieza por `*`), basta con que coincida uno.

use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq)]
pub struct Schedule {
    minute: u64,
    hour: u64,
    dom: u64,
    month: u64,
    dow: u64,
    dom_star: bool,
    dow_star: bool,
}

/// Un instante a resolución de minuto, en UTC.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Minute {
    pub minute: u32,
    pub hour: u32,
    pub dom: u32,
    pub month: u32,
    /// 0 = domingo.
    pub dow: u32,
}

/// Bits `min..=max` de un campo.
fn parse_field(field: &str, min: u32, max: u32) -> Result<u64, String> {
    let mut mask = 0u64;
    for part in field.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => {
                let s: u32 = s.parse().map_err(|_| format!("invalid step in {part:?}"))?;
                if s == 0 {
                    return Err(format!("zero step in {part:?}"));
                }
                (r, Some(s))
            }
            None => (part, None),
        };
        let num = |s: &str| -> Result<u32, String> {
            let n: u32 = s.parse().map_err(|_| format!("invalid value {s:?}"))?;
            if n < min || n > max {
                return Err(format!("{n} out of range {min}-{max}"));
            }
            Ok(n)
        };
        let (lo, hi) = if range == "*" {
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            let (a, b) = (num(a)?, num(b)?);
            if a > b {
                return Err(format!("reversed range {range:?}"));
            }
            (a, b)
        } else {
            let a = num(range)?;
            // `5/10` va de 5 al máximo; `5` es solo 5.
            (a, if step.is_some() { max } else { a })
        };
        for v in (lo..=hi).step_by(step.unwrap_or(1) as usize) {
            mask |= 1 << v;
        }
    }
    Ok(mask)
}

impl Schedule {
    pub fn parse(expr: &str) -> Result<Self, String> {
        if expr.len() > 100 {
            return Err("schedule longer than 100 characters".into());
        }
        let fields: Vec<&str> = expr.split_whitespace().collect();
        let [mi, h, dom, mo, dow] = fields[..] else {
            return Err("schedule must have 5 fields".into());
        };
        let mut dow_mask = parse_field(dow, 0, 7)?;
        if dow_mask & (1 << 7) != 0 {
            dow_mask = (dow_mask | 1) & !(1 << 7);
        }
        Ok(Self {
            minute: parse_field(mi, 0, 59)?,
            hour: parse_field(h, 0, 23)?,
            dom: parse_field(dom, 1, 31)?,
            month: parse_field(mo, 1, 12)?,
            dow: dow_mask,
            dom_star: dom.starts_with('*'),
            dow_star: dow.starts_with('*'),
        })
    }

    pub fn matches(&self, t: Minute) -> bool {
        let bit = |mask: u64, v: u32| mask & (1 << v) != 0;
        let dom = bit(self.dom, t.dom);
        let dow = bit(self.dow, t.dow);
        let day = if self.dom_star || self.dow_star { dom && dow } else { dom || dow };
        bit(self.minute, t.minute) && bit(self.hour, t.hour) && bit(self.month, t.month) && day
    }
}

impl Minute {
    /// Desglose UTC de segundos Unix (algoritmo `civil_from_days` de H. Hinnant).
    pub fn from_unix(secs: u64) -> Self {
        let days = secs / 86_400;
        let rem = secs % 86_400;
        let z = days as i64 + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let dom = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
        Self {
            minute: (rem / 60 % 60) as u32,
            hour: (rem / 3600) as u32,
            dom,
            month,
            dow: ((days + 4) % 7) as u32, // 1970-01-01 fue jueves
        }
    }

    pub fn now() -> Self {
        Self::from_unix(SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(minute: u32, hour: u32, dom: u32, month: u32, dow: u32) -> Minute {
        Minute { minute, hour, dom, month, dow }
    }

    fn values(field: &str, min: u32, max: u32) -> Vec<u32> {
        let m = parse_field(field, min, max).unwrap();
        (min..=max).filter(|v| m & (1 << v) != 0).collect()
    }

    #[test]
    fn field_syntax() {
        assert_eq!(values("*", 0, 5), vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(values("3", 0, 59), vec![3]);
        assert_eq!(values("1,5,9", 0, 59), vec![1, 5, 9]);
        assert_eq!(values("10-13", 0, 59), vec![10, 11, 12, 13]);
        assert_eq!(values("*/15", 0, 59), vec![0, 15, 30, 45]);
        assert_eq!(values("1-10/3", 0, 59), vec![1, 4, 7, 10]);
        assert_eq!(values("50/4", 0, 59), vec![50, 54, 58]);
        assert_eq!(values("*/5", 1, 12), vec![1, 6, 11]);
        assert_eq!(values("1-3,20-22/2,59", 0, 59), vec![1, 2, 3, 20, 22, 59]);
    }

    #[test]
    fn rejects_bad_expressions() {
        for bad in [
            "", "* * * *", "* * * * * *", "60 * * * *", "* 24 * * *", "* * 0 * *",
            "* * 32 * *", "* * * 0 *", "* * * 13 *", "* * * * 8", "*/0 * * * *",
            "5-1 * * * *", "a * * * *", "1,,2 * * * *", "-1 * * * *", "1- * * * *",
            "*/ * * * *", "@daily", "* * * JAN *", "* * * * MON", "1/2/3 * * * *",
        ] {
            assert!(Schedule::parse(bad).is_err(), "{bad:?} should be rejected");
        }
        assert!(Schedule::parse(&format!("0 0 * * *{}", " ".repeat(100))).is_err());
    }

    #[test]
    fn matching() {
        let s = Schedule::parse("*/15 9-17 * * 1-5").unwrap();
        assert!(s.matches(at(30, 9, 14, 10, 3)));
        assert!(!s.matches(at(31, 9, 14, 10, 3)));
        assert!(!s.matches(at(30, 18, 14, 10, 3)));
        assert!(!s.matches(at(30, 9, 18, 10, 6)), "saturday");

        let every = Schedule::parse("* * * * *").unwrap();
        assert!(every.matches(at(59, 23, 31, 12, 0)));

        let monthly = Schedule::parse("0 3 1 */3 *").unwrap();
        assert!(monthly.matches(at(0, 3, 1, 4, 2)));
        assert!(!monthly.matches(at(0, 3, 1, 5, 2)));
    }

    #[test]
    fn sunday_is_0_and_7() {
        for e in ["0 0 * * 0", "0 0 * * 7", "0 0 * * 5-7"] {
            let s = Schedule::parse(e).unwrap();
            assert!(s.matches(at(0, 0, 18, 10, 0)), "{e}");
            assert!(!s.matches(at(0, 0, 14, 10, 3)), "{e}");
        }
    }

    #[test]
    fn day_of_month_and_week_use_or_when_both_restricted() {
        // Día 1 o cualquier lunes.
        let s = Schedule::parse("0 0 1 * 1").unwrap();
        assert!(s.matches(at(0, 0, 1, 10, 4)));
        assert!(s.matches(at(0, 0, 13, 10, 1)));
        assert!(!s.matches(at(0, 0, 14, 10, 2)));
        // Con uno de los dos en `*` es AND (y `*/2` cuenta como `*`).
        let s = Schedule::parse("0 0 1 * *").unwrap();
        assert!(!s.matches(at(0, 0, 13, 10, 1)));
        let s = Schedule::parse("0 0 */2 * 1").unwrap();
        assert!(s.matches(at(0, 0, 13, 10, 1)));
        assert!(!s.matches(at(0, 0, 12, 10, 1)), "even day");
        assert!(!s.matches(at(0, 0, 13, 10, 2)), "not monday");
    }

    #[test]
    fn unix_to_utc_fields() {
        assert_eq!(Minute::from_unix(0), at(0, 0, 1, 1, 4));
        // 2026-10-15T13:47:30Z, jueves.
        assert_eq!(Minute::from_unix(1_792_072_050), at(47, 13, 15, 10, 4));
        // 2024-02-29T23:59:00Z, jueves (bisiesto).
        assert_eq!(Minute::from_unix(1_709_251_140), at(59, 23, 29, 2, 4));
        // 2000-03-01T00:00:00Z, miércoles.
        assert_eq!(Minute::from_unix(951_868_800), at(0, 0, 1, 3, 3));
    }
}
