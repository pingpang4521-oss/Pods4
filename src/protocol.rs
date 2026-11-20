pub const SOM: u8 = 0xFD;
pub const EOM: u8 = 0xDD;

/// Samsung Galaxy Buds SPP UUID: 2e73a4ad-332d-41fc-90e2-16bef06523f2 (Windows GUID byte layout)
pub const SAMSUNG_SPP_GUID: [u8; 16] = [
    0xad, 0xa4, 0x73, 0x2e, // Data1 (LE)
    0x2d, 0x33,             // Data2 (LE)
    0xfc, 0x41,             // Data3 (LE)
    0x90, 0xe2, 0x16, 0xbe, 0xf0, 0x65, 0x23, 0xf2, // Data4
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NoiseControlMode {
    Off = 0,
    Anc = 1,
    Ambient = 2,
    Adaptive = 3,
}

impl NoiseControlMode {
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(Self::Off),
            1 => Some(Self::Anc),
            2 => Some(Self::Ambient),
            3 => Some(Self::Adaptive),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Anc => "ANC",
            Self::Ambient => "Ambient",
            Self::Adaptive => "Adaptive",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlacementState {
    #[default]
    Disconnected,
    Wearing,
    Idle,
    Case,
    ClosedCase,
    Charging,
}

impl PlacementState {
    pub fn from_byte(b: u8) -> Self {
        match b {
            1 => Self::Wearing,
            2 => Self::Idle,
            3 => Self::Case,
            4 => Self::ClosedCase,
            100 => Self::Charging,
            _ => Self::Disconnected,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Wearing => "In Ear",
            Self::Idle => "Out of Ear",
            Self::Case => "In Case",
            Self::ClosedCase => "Case Closed",
            Self::Charging => "Charging",
            Self::Disconnected => "Offline",
        }
    }
}

pub mod msg_id {
    pub const UNIVERSAL_ACK: u8 = 0x42;              // 66
    pub const STATUS_UPDATED: u8 = 0x60;             // 96
    pub const EXTENDED_STATUS_UPDATED: u8 = 0x61;    // 97
    pub const SET_ANC_WITH_ONE_EARBUD: u8 = 0x6F;    // 111
    pub const NOISE_CONTROLS_UPDATE: u8 = 0x77;      // 119
    pub const NOISE_CONTROLS: u8 = 0x78;             // 120
    pub const SET_DETECT_CONVERSATIONS: u8 = 0x7A;   // 122
    pub const SET_DETECT_CONVERSATIONS_DUR: u8 = 0x7B; // 123
    pub const CUSTOMIZE_AMBIENT_SOUND: u8 = 0x82;    // 130
    pub const NOISE_REDUCTION_LEVEL: u8 = 0x83;      // 131
    pub const AMBIENT_VOLUME: u8 = 0x84;             // 132
    pub const ADJUST_SOUND_SYNC: u8 = 0x85;          // 133 (Gaming Mode / low-latency sync)
    pub const SET_SIDETONE: u8 = 0x8B;               // 139
    pub const SIREN_DETECT: u8 = 0xDE;               // 222
}

#[derive(Debug, Clone)]
pub struct SppPacket {
    pub id: u8,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct BudsState {
    pub mode: NoiseControlMode,
    pub ambient_volume: u8,        // 0..=4 (Levels 1..5)
    pub anc_level: u8,             // 0..=4 (Levels 1..5)

    pub custom_ambient_on: bool,
    pub custom_ambient_left: u8,   // 0..=2 (-1, Normal, +1)
    pub custom_ambient_right: u8,  // 0..=2 (-1, Normal, +1)
    pub custom_ambient_tone: u8,   // 0..=4 (Soft +2 .. Clear +2)

    pub voice_detect: bool,
    pub voice_detect_duration: u8, // 0=5s, 1=10s, 2=15s
    pub siren_detect: bool,
    pub ambient_during_calls: bool,
    pub one_earbud_noise_control: bool,
    pub gaming_mode: bool,

    pub battery_l: u8,
    pub battery_r: u8,
    pub battery_case: u8,
    pub placement_l: PlacementState,
    pub placement_r: PlacementState,
    pub is_left_charging: bool,
    pub is_right_charging: bool,
    pub is_case_charging: bool,
    pub has_initial_sync: bool,
}

impl Default for BudsState {
    fn default() -> Self {
        Self {
            mode: NoiseControlMode::Off,
            ambient_volume: 2,
            anc_level: 4,
            custom_ambient_on: false,
            custom_ambient_left: 1,
            custom_ambient_right: 1,
            custom_ambient_tone: 2,
            voice_detect: false,
            voice_detect_duration: 0,
            siren_detect: false,
            ambient_during_calls: false,
            one_earbud_noise_control: false,
            gaming_mode: false,
            battery_l: 0,
            battery_r: 0,
            battery_case: 0,
            placement_l: PlacementState::Disconnected,
            placement_r: PlacementState::Disconnected,
            is_left_charging: false,
            is_right_charging: false,
            is_case_charging: false,
            has_initial_sync: false,
        }
    }
}

const CRC_TAB: [u16; 256] = [
    0, 4129, 8258, 12387, 16516, 20645, 24774, 28903, 33032, 37161, 41290, 45419, 49548, 53677, 57806, 61935,
    4657, 528, 12915, 8786, 21173, 17044, 29431, 25302, 37689, 33560, 45947, 41818, 54205, 50076, 62463, 58334,
    9314, 13379, 1056, 5121, 25830, 29895, 17572, 21637, 42346, 46411, 34088, 38153, 58862, 62927, 50604, 54669,
    13907, 9842, 5649, 1584, 30423, 26358, 22165, 18100, 46939, 42874, 38681, 34616, 63455, 59390, 55197, 51132,
    18628, 22757, 26758, 30887, 2112, 6241, 10242, 14371, 51660, 55789, 59790, 63919, 35144, 39273, 43274, 47403,
    23285, 19156, 31415, 27286, 6769, 2640, 14899, 10770, 56317, 52188, 64447, 60318, 39801, 35672, 47931, 43802,
    27814, 31879, 19684, 23749, 11298, 15363, 3168, 7233, 60846, 64911, 52716, 56781, 44330, 48395, 36200, 40265,
    32407, 28342, 24277, 20212, 15891, 11826, 7761, 3696, 65439, 61374, 57309, 53244, 48923, 44858, 40793, 36728,
    37256, 33193, 45514, 41451, 53516, 49453, 61774, 57711, 4224, 161, 12482, 8419, 20484, 16421, 28742, 24679,
    33721, 37784, 41979, 46042, 49981, 54044, 58239, 62302, 689, 4752, 8947, 13010, 16949, 21012, 25207, 29270,
    46570, 42443, 38312, 34185, 62830, 58703, 54572, 50445, 13538, 9411, 5280, 1153, 29798, 25671, 21540, 17413,
    42971, 47098, 34713, 38840, 59231, 63358, 50973, 55100, 9939, 14066, 1681, 5808, 26199, 30326, 17941, 22068,
    55628, 51565, 63758, 59695, 39368, 35305, 47498, 43435, 22596, 18533, 30726, 26663, 6336, 2273, 14466, 10403,
    52093, 56156, 60223, 64286, 35833, 39896, 43963, 48026, 19061, 23124, 27191, 31254, 2801, 6864, 10931, 14994,
    64814, 60687, 56684, 52557, 48554, 44427, 40424, 36297, 31782, 27655, 23652, 19525, 15522, 11395, 7392, 3265,
    61215, 65342, 53085, 57212, 44955, 49082, 36825, 40952, 28183, 32310, 20053, 24180, 11923, 16050, 3793, 7920,
];

pub fn compute_crc(id: u8, payload: &[u8]) -> u16 {
    let mut crc = CRC_TAB[id as usize];
    for &b in payload {
        crc = CRC_TAB[(((crc >> 8) as u8) ^ b) as usize] ^ (crc << 8);
    }
    crc
}

pub fn encode(id: u8, payload: &[u8]) -> Vec<u8> {
    let size = 1 + payload.len() + 2;
    let mut frame = Vec::with_capacity(4 + size);
    frame.push(SOM);
    frame.push((size & 0xFF) as u8);
    frame.push(((size >> 8) & 0xFF) as u8);
    frame.push(id);
    frame.extend_from_slice(payload);
    let crc = compute_crc(id, payload);
    frame.push((crc & 0xFF) as u8);
    frame.push(((crc >> 8) & 0xFF) as u8);
    frame.push(EOM);
    frame
}

pub fn set_noise_control(mode: NoiseControlMode) -> Vec<u8> {
    encode(msg_id::NOISE_CONTROLS, &[mode as u8])
}

pub fn set_ambient_volume(level: u8) -> Vec<u8> {
    encode(msg_id::AMBIENT_VOLUME, &[level.min(4)])
}

pub fn set_anc_level(level: u8) -> Vec<u8> {
    encode(msg_id::NOISE_REDUCTION_LEVEL, &[level.min(4)])
}

pub fn set_voice_detect(on: bool) -> Vec<u8> {
    encode(msg_id::SET_DETECT_CONVERSATIONS, &[u8::from(on)])
}

pub fn set_voice_detect_duration(idx: u8) -> Vec<u8> {
    encode(msg_id::SET_DETECT_CONVERSATIONS_DUR, &[idx.min(2)])
}

pub fn set_siren_detect(on: bool) -> Vec<u8> {
    encode(msg_id::SIREN_DETECT, &[u8::from(on)])
}

pub fn set_ambient_during_calls(on: bool) -> Vec<u8> {
    encode(msg_id::SET_SIDETONE, &[u8::from(on)])
}

pub fn set_one_earbud_noise_control(on: bool) -> Vec<u8> {
    encode(msg_id::SET_ANC_WITH_ONE_EARBUD, &[u8::from(on)])
}

pub fn set_gaming_mode(on: bool) -> Vec<u8> {
    encode(msg_id::ADJUST_SOUND_SYNC, &[u8::from(on)])
}

pub fn set_customize_ambient(on: bool, left: u8, right: u8, tone: u8) -> Vec<u8> {
    encode(
        msg_id::CUSTOMIZE_AMBIENT_SOUND,
        &[u8::from(on), left.min(2), right.min(2), tone.min(4)],
    )
}

/// Extracts all complete valid SPP packets from a rolling receive buffer, keeping incomplete trailing bytes.
pub fn extract_packets(buf: &mut Vec<u8>) -> Vec<SppPacket> {
    let mut out = Vec::new();
    while !buf.is_empty() {
        let Some(som_idx) = buf.iter().position(|&b| b == SOM) else {
            buf.clear();
            break;
        };
        if som_idx > 0 {
            buf.drain(..som_idx);
        }
        if buf.len() < 7 {
            break;
        }

        let header = (buf[1] as usize) | ((buf[2] as usize) << 8);
        let size = header & 0x3FF;
        if size < 3 {
            buf.remove(0);
            continue;
        }

        let total_len = 3 + size + 1;
        if buf.len() < total_len {
            break;
        }

        let payload_len = size - 3;
        let id = buf[3];
        let payload = &buf[4..4 + payload_len];
        let expected_crc = compute_crc(id, payload);
        let crc_lo = buf[4 + payload_len];
        let crc_hi = buf[5 + payload_len];
        let eom = buf[6 + payload_len];

        if eom == EOM && crc_lo == (expected_crc & 0xFF) as u8 && crc_hi == ((expected_crc >> 8) & 0xFF) as u8 {
            out.push(SppPacket { id, payload: payload.to_vec() });
            buf.drain(..total_len);
        } else {
            buf.remove(0);
        }
    }
    out
}

pub fn apply_packet(state: &mut BudsState, pkt: &SppPacket) -> bool {
    let p = &pkt.payload;
    match pkt.id {
        msg_id::EXTENDED_STATUS_UPDATED if p.len() >= 34 => {
            state.battery_l = p[2];
            state.battery_r = p[3];
            state.placement_l = PlacementState::from_byte((p[6] & 0xF0) >> 4);
            state.placement_r = PlacementState::from_byte(p[6] & 0x0F);
            state.battery_case = p[7];
            state.gaming_mode = p[8] == 1;
            state.mode = NoiseControlMode::from_byte(p[12]).unwrap_or(NoiseControlMode::Off);
            state.ambient_volume = p[23].min(4);
            state.anc_level = p[24].min(4);
            state.voice_detect = p[26] == 1;
            state.voice_detect_duration = if p[27] <= 2 { p[27] } else { 0 };
            state.one_earbud_noise_control = p[28] == 1;
            state.custom_ambient_on = p[29] == 1;
            state.custom_ambient_left = ((p[30] & 0xF0) >> 4).min(2);
            state.custom_ambient_right = (p[30] & 0x0F).min(2);
            state.custom_ambient_tone = p[31].min(4);
            state.ambient_during_calls = p[33] == 1;

            if p.len() > 42 {
                let chg = p[42];
                state.is_left_charging = (chg & 16) != 0;
                state.is_right_charging = (chg & 4) != 0;
                state.is_case_charging = (chg & 1) != 0;
                if state.is_left_charging { state.placement_l = PlacementState::Charging; }
                if state.is_right_charging { state.placement_r = PlacementState::Charging; }
            }
            if p.len() > 52 {
                state.siren_detect = p[52] == 1;
            }
            state.has_initial_sync = true;
            true
        }
        msg_id::STATUS_UPDATED if p.len() >= 7 => {
            state.battery_l = p[1];
            state.battery_r = p[2];
            state.placement_l = PlacementState::from_byte((p[5] & 0xF0) >> 4);
            state.placement_r = PlacementState::from_byte(p[5] & 0x0F);
            state.battery_case = p[6];
            if p.len() > 7 {
                let chg = p[7];
                state.is_left_charging = (chg & 16) != 0;
                state.is_right_charging = (chg & 4) != 0;
                state.is_case_charging = (chg & 1) != 0;
                if state.is_left_charging { state.placement_l = PlacementState::Charging; }
                if state.is_right_charging { state.placement_r = PlacementState::Charging; }
            }
            true
        }
        msg_id::NOISE_CONTROLS_UPDATE if !p.is_empty() => {
            if let Some(m) = NoiseControlMode::from_byte(p[0]) {
                state.mode = m;
                return true;
            }
            false
        }
        msg_id::AMBIENT_VOLUME if !p.is_empty() => {
            state.ambient_volume = p[0].min(4);
            true
        }
        msg_id::UNIVERSAL_ACK if p.len() >= 2 => {
            let ack_id = p[0];
            let val = p[1];
            match ack_id {
                msg_id::NOISE_CONTROLS => {
                    if let Some(m) = NoiseControlMode::from_byte(val) {
                        state.mode = m;
                        return true;
                    }
                    false
                }
                msg_id::AMBIENT_VOLUME => { state.ambient_volume = val.min(4); true }
                msg_id::NOISE_REDUCTION_LEVEL => { state.anc_level = val.min(4); true }
                msg_id::SET_DETECT_CONVERSATIONS => { state.voice_detect = val == 1; true }
                msg_id::SET_DETECT_CONVERSATIONS_DUR => { state.voice_detect_duration = val.min(2); true }
                msg_id::SIREN_DETECT => { state.siren_detect = val == 1; true }
                msg_id::SET_SIDETONE => { state.ambient_during_calls = val == 1; true }
                msg_id::SET_ANC_WITH_ONE_EARBUD => { state.one_earbud_noise_control = val == 1; true }
                msg_id::ADJUST_SOUND_SYNC => { state.gaming_mode = val == 1; true }
                msg_id::CUSTOMIZE_AMBIENT_SOUND if p.len() >= 5 => {
                    state.custom_ambient_on = p[1] == 1;
                    state.custom_ambient_left = p[2].min(2);
                    state.custom_ambient_right = p[3].min(2);
                    state.custom_ambient_tone = p[4].min(4);
                    true
                }
                _ => false,
            }
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn decodes_live_buds4_pro_extended_status_and_ack() {
        let live_ext = hex(
            "FD410061040D1B64010021000000FF2200006801680103000499000404100000\
             00001102000000000000000000000000000000000101000000FF010100000100\
             00011029DD",
        );
        let live_ack = hex("FD055042840415E4DD");

        // Chunked stream test
        let mut buf = live_ext[..20].to_vec();
        assert!(extract_packets(&mut buf).is_empty());
        buf.extend_from_slice(&live_ext[20..]);
        buf.extend_from_slice(&live_ack);

        let pkts = extract_packets(&mut buf);
        assert_eq!(pkts.len(), 2);
        assert!(buf.is_empty());

        let mut st = BudsState::default();
        assert!(apply_packet(&mut st, &pkts[0]));
        assert_eq!(st.battery_l, 27);
        assert_eq!(st.battery_r, 100);
        assert_eq!(st.placement_l, PlacementState::Idle);
        assert_eq!(st.placement_r, PlacementState::Wearing);
        assert_eq!(st.mode, NoiseControlMode::Off);
        assert_eq!(st.ambient_volume, 4);
        assert_eq!(st.anc_level, 4);
        assert_eq!((st.custom_ambient_left, st.custom_ambient_right, st.custom_ambient_tone), (1, 1, 2));
        assert!(st.has_initial_sync);

        // ACK packet also applies cleanly
        assert!(apply_packet(&mut st, &pkts[1]));
        assert_eq!(st.ambient_volume, 4);
    }

    #[test]
    fn command_encoders_round_trip() {
        let cmds = [
            set_noise_control(NoiseControlMode::Ambient),
            set_noise_control(NoiseControlMode::Adaptive),
            set_ambient_volume(3),
            set_anc_level(2),
            set_voice_detect(true),
            set_voice_detect_duration(1),
            set_siren_detect(true),
            set_ambient_during_calls(true),
            set_one_earbud_noise_control(true),
            set_gaming_mode(true),
            set_customize_ambient(true, 2, 0, 4),
        ];
        for mut cmd in cmds {
            let decoded = extract_packets(&mut cmd);
            assert_eq!(decoded.len(), 1);
            assert!(cmd.is_empty());
        }

        let mut st = BudsState::default();
        let mut ack = encode(msg_id::UNIVERSAL_ACK, &[msg_id::NOISE_CONTROLS, NoiseControlMode::Ambient as u8]);
        let pkt = extract_packets(&mut ack).pop().unwrap();
        assert!(apply_packet(&mut st, &pkt));
        assert_eq!(st.mode, NoiseControlMode::Ambient);

        let mut siren = encode(msg_id::UNIVERSAL_ACK, &[msg_id::SIREN_DETECT, 1]);
        let pkt = extract_packets(&mut siren).pop().unwrap();
        assert!(apply_packet(&mut st, &pkt));
        assert!(st.siren_detect);

        let mut gm = encode(msg_id::UNIVERSAL_ACK, &[msg_id::ADJUST_SOUND_SYNC, 1]);
        let pkt = extract_packets(&mut gm).pop().unwrap();
        assert!(apply_packet(&mut st, &pkt));
        assert!(st.gaming_mode);
    }
}
