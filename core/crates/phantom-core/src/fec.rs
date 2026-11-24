//! Parità XOR adattiva: quanti frammenti di parità mandare in base alle perdite che il client riporta.
//!
//! Fino alla 1.5.0 la parità era fissa (un frammento ogni 10). In LAN pulita basta e avanza; su 5G e via relay, dove
//! si perde qualche pacchetto in modo sparso, un gruppo da 10 recupera troppo poco e ogni frame rotto costa un giro di
//! NACK o, peggio, un keyframe. Qui il gruppo si stringe quando i frame arrivano danneggiati (persi o salvati proprio
//! dalla parità) e torna largo dopo `RELAX_AFTER_US` di rete pulita.
//!
//! Le perdite da congestione non si curano con più parità (aggiunge byte a un collegamento già pieno): il gruppo si
//! stringe solo quando il client misura una coda bassa (le perdite sono della radio, non del carico) e torna subito
//! largo appena il controllo del bitrate vede congestione.

/// Frazione di frame danneggiati (persi o recuperati dalla parità) da cui si passa al gruppo da 5.
pub const MEDIUM_IMPAIRED: f64 = 0.03;
/// Frazione di frame danneggiati da cui si passa al gruppo da 3.
pub const HIGH_IMPAIRED: f64 = 0.10;
/// Gruppo per perdite medie (20% di parità).
pub const MEDIUM_GROUP: u16 = 5;
/// Gruppo per perdite alte (33% di parità).
pub const HIGH_GROUP: u16 = 3;
/// Rete pulita per questo tempo prima di allargare il gruppo (µs).
pub const RELAX_AFTER_US: u64 = 10_000_000;
/// Frame minimi in una finestra per giudicarla (con meno si aspetta la statistica successiva).
pub const MIN_FRAMES: u32 = 20;

/// Stato della rete secondo il controllo del bitrate, per quel che serve alla parità.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Load {
    /// Coda misurata e bassa: le perdite non vengono dal carico.
    Clear,
    /// Non si sa (coda a metà o non misurata).
    Unknown,
    /// Congestione o perdite catastrofiche: niente parità in più.
    Loaded,
}

/// Sceglie la dimensione del gruppo di parità a partire dalle statistiche del client.
#[derive(Clone, Debug)]
pub struct AdaptiveFec {
    base: u16,
    current: u16,
    prev: Option<(u32, u32, u32)>,
    calm_since_us: Option<u64>,
}

impl AdaptiveFec {
    /// `base` è il gruppo con la rete pulita (0 = parità spenta: resta spenta).
    pub fn new(base: u16) -> Self {
        AdaptiveFec { base, current: base, prev: None, calm_since_us: None }
    }

    /// Gruppo attuale.
    pub fn group(&self) -> u16 {
        self.current
    }

    /// Statistiche cumulative del client (frame consegnati, persi, recuperati dalla parità) e stato della rete. Il
    /// gruppo si stringe solo con [`Load::Clear`] e torna alla base con [`Load::Loaded`]. Restituisce il nuovo gruppo
    /// se cambia.
    pub fn on_stats(&mut self, now_us: u64, ok: u32, lost: u32, fec: u32, load: Load) -> Option<u16> {
        if self.base == 0 {
            return None;
        }
        if load == Load::Loaded && self.current != self.base {
            // la parità in più pesa su un collegamento già pieno: via subito
            self.current = self.base;
            self.calm_since_us = None;
            self.prev = Some((ok, lost, fec));
            return Some(self.base);
        }
        let Some((p_ok, p_lost, p_fec)) = self.prev else {
            self.prev = Some((ok, lost, fec));
            return None;
        };
        let d_ok = ok.wrapping_sub(p_ok);
        let d_lost = lost.wrapping_sub(p_lost);
        let d_fec = fec.wrapping_sub(p_fec);
        let total = d_ok.saturating_add(d_lost);
        if total < MIN_FRAMES {
            // finestra troppo corta: si accumula con la prossima
            return None;
        }
        self.prev = Some((ok, lost, fec));
        let impaired = f64::from(d_lost.saturating_add(d_fec).min(total)) / f64::from(total);
        let wanted = if impaired >= HIGH_IMPAIRED {
            HIGH_GROUP.min(self.base)
        } else if impaired >= MEDIUM_IMPAIRED {
            MEDIUM_GROUP.min(self.base)
        } else {
            self.base
        };
        if wanted < self.current {
            self.calm_since_us = None;
            if load != Load::Clear {
                return None;
            }
            self.current = wanted;
            return Some(wanted);
        }
        if wanted > self.current {
            let since = *self.calm_since_us.get_or_insert(now_us);
            if now_us.saturating_sub(since) >= RELAX_AFTER_US {
                // un gradino alla volta: da 3 a 5, poi da 5 alla base
                self.current =
                    if self.current < MEDIUM_GROUP.min(self.base) { MEDIUM_GROUP.min(self.base) } else { wanted };
                self.calm_since_us = None;
                return Some(self.current);
            }
            return None;
        }
        self.calm_since_us = None;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simula una statistica al secondo con 60 frame e la frazione di frame danneggiati data.
    fn feed(f: &mut AdaptiveFec, cum: &mut (u32, u32, u32), t_s: u64, damaged: u32, load: Load) -> Option<u16> {
        cum.0 += 60 - damaged / 2;
        cum.1 += damaged / 2;
        cum.2 += damaged - damaged / 2;
        f.on_stats(t_s * 1_000_000, cum.0, cum.1, cum.2, load)
    }

    #[test]
    fn tightens_quickly_and_relaxes_slowly() {
        let mut f = AdaptiveFec::new(10);
        let mut c = (0, 0, 0);
        assert_eq!(feed(&mut f, &mut c, 0, 0, Load::Clear), None, "prima statistica: solo riferimento");
        assert_eq!(feed(&mut f, &mut c, 1, 0, Load::Clear), None);
        // 5% dei frame danneggiati: gruppo da 5 subito
        assert_eq!(feed(&mut f, &mut c, 2, 3, Load::Clear), Some(5));
        // 15%: gruppo da 3
        assert_eq!(feed(&mut f, &mut c, 3, 9, Load::Clear), Some(3));
        // rete pulita: si allarga solo dopo 10 s, un gradino alla volta
        for t in 4..14 {
            assert_eq!(feed(&mut f, &mut c, t, 0, Load::Clear), None, "secondo {t}");
        }
        assert_eq!(feed(&mut f, &mut c, 14, 0, Load::Clear), Some(5));
        for t in 15..25 {
            assert_eq!(feed(&mut f, &mut c, t, 0, Load::Clear), None);
        }
        assert_eq!(feed(&mut f, &mut c, 25, 0, Load::Clear), Some(10));
        assert_eq!(f.group(), 10);
    }

    #[test]
    fn congestion_does_not_add_parity_and_short_windows_accumulate() {
        let mut f = AdaptiveFec::new(10);
        let mut c = (0, 0, 0);
        feed(&mut f, &mut c, 0, 0, Load::Clear);
        assert_eq!(feed(&mut f, &mut c, 1, 12, Load::Loaded), None, "perdite da coda: niente parità in più");
        assert_eq!(feed(&mut f, &mut c, 2, 12, Load::Unknown), None, "coda non chiara: nemmeno");
        assert_eq!(f.group(), 10);
        assert_eq!(feed(&mut f, &mut c, 3, 12, Load::Clear), Some(3));
        assert_eq!(
            feed(&mut f, &mut c, 4, 12, Load::Loaded),
            Some(10),
            "arriva la congestione: si torna subito larghi"
        );
        // finestre da 10 frame: si giudicano insieme
        assert_eq!(f.on_stats(5_000_000, c.0 + 9, c.1 + 1, c.2, Load::Clear), None);
        assert_eq!(f.on_stats(5_100_000, c.0 + 18, c.1 + 2, c.2, Load::Clear), Some(3), "2 persi su 20 = 10%");
    }

    #[test]
    fn disabled_parity_stays_disabled() {
        let mut f = AdaptiveFec::new(0);
        let mut c = (0, 0, 0);
        for t in 0..5 {
            assert_eq!(feed(&mut f, &mut c, t, 20, Load::Clear), None);
        }
        assert_eq!(f.group(), 0);
    }
}
