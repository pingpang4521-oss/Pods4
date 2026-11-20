using System.Diagnostics;

namespace Pods4;

public enum NoiseControlMode : byte
{
    Off = 0,
    Anc = 1,
    Ambient = 2,
    Adaptive = 3
}

public enum PlacementState : byte
{
    Disconnected = 0,
    Wearing = 1,
    Idle = 2,
    Case = 3,
    ClosedCase = 4,
    Charging = 100
}

public enum MsgId : byte
{
    UniversalAck = 0x42,               // 66
    StatusUpdated = 0x60,              // 96
    ExtendedStatusUpdated = 0x61,      // 97
    SetAncWithOneEarbud = 0x6F,        // 111
    NoiseControlsUpdate = 0x77,        // 119
    NoiseControls = 0x78,              // 120
    SetDetectConversations = 0x7A,     // 122
    SetDetectConversationsDur = 0x7B,  // 123
    CustomizeAmbientSound = 0x82,      // 130
    NoiseReductionLevel = 0x83,        // 131
    AmbientVolume = 0x84,              // 132
    SetSidetone = 0x8B,                // 139
    SirenDetect = 0xDE                 // 222
}

public sealed record SppPacket(MsgId Id, byte[] Payload);

public sealed class BudsState
{
    public NoiseControlMode Mode { get; set; } = NoiseControlMode.Off;
    public int AmbientVolume { get; set; } = 2;       // 0..4 (Levels 1..5)
    public int AncLevel { get; set; } = 4;            // 0..4 (Levels 1..5)

    public bool CustomAmbientOn { get; set; }
    public int CustomAmbientLeft { get; set; } = 1;   // 0..2 (-1, Normal, +1)
    public int CustomAmbientRight { get; set; } = 1;  // 0..2 (-1, Normal, +1)
    public int CustomAmbientTone { get; set; } = 2;   // 0..4 (Soft +2 .. Clear +2)

    public bool VoiceDetect { get; set; }
    public int VoiceDetectDuration { get; set; }      // 0=5s, 1=10s, 2=15s
    public bool SirenDetect { get; set; }
    public bool AmbientDuringCalls { get; set; }
    public bool OneEarbudNoiseControl { get; set; }

    public int BatteryL { get; set; }
    public int BatteryR { get; set; }
    public int BatteryCase { get; set; }
    public PlacementState PlacementL { get; set; }
    public PlacementState PlacementR { get; set; }
    public bool IsLeftCharging { get; set; }
    public bool IsRightCharging { get; set; }
    public bool IsCaseCharging { get; set; }
    public bool HasInitialSync { get; set; }

    public BudsState Clone() => (BudsState)MemberwiseClone();
}

public static class BudsProtocol
{
    public const byte Som = 0xFD;
    public const byte Eom = 0xDD;
    public static readonly Guid SamsungSppUuid = new("2e73a4ad-332d-41fc-90e2-16bef06523f2");

    private static readonly ushort[] CrcTab =
    [
        0, 4129, 8258, 12387, 16516, 20645, 24774, 28903, 33032, 37161, 41290,
        45419, 49548, 53677, 57806, 61935, 4657, 528, 12915, 8786, 21173, 17044, 29431, 25302,
        37689, 33560, 45947, 41818, 54205, 50076, 62463, 58334, 9314, 13379, 1056, 5121, 25830,
        29895, 17572, 21637, 42346, 46411, 34088, 38153, 58862, 62927, 50604, 54669, 13907,
        9842, 5649, 1584, 30423, 26358, 22165, 18100, 46939, 42874, 38681, 34616, 63455, 59390,
        55197, 51132, 18628, 22757, 26758, 30887, 2112, 6241, 10242, 14371, 51660, 55789,
        59790, 63919, 35144, 39273, 43274, 47403, 23285, 19156, 31415, 27286, 6769, 2640,
        14899, 10770, 56317, 52188, 64447, 60318, 39801, 35672, 47931, 43802, 27814, 31879,
        19684, 23749, 11298, 15363, 3168, 7233, 60846, 64911, 52716, 56781, 44330, 48395,
        36200, 40265, 32407, 28342, 24277, 20212, 15891, 11826, 7761, 3696, 65439, 61374,
        57309, 53244, 48923, 44858, 40793, 36728, 37256, 33193, 45514, 41451, 53516, 49453,
        61774, 57711, 4224, 161, 12482, 8419, 20484, 16421, 28742, 24679, 33721, 37784, 41979,
        46042, 49981, 54044, 58239, 62302, 689, 4752, 8947, 13010, 16949, 21012, 25207, 29270,
        46570, 42443, 38312, 34185, 62830, 58703, 54572, 50445, 13538, 9411, 5280, 1153, 29798,
        25671, 21540, 17413, 42971, 47098, 34713, 38840, 59231, 63358, 50973, 55100, 9939,
        14066, 1681, 5808, 26199, 30326, 17941, 22068, 55628, 51565, 63758, 59695, 39368,
        35305, 47498, 43435, 22596, 18533, 30726, 26663, 6336, 2273, 14466, 10403, 52093,
        56156, 60223, 64286, 35833, 39896, 43963, 48026, 19061, 23124, 27191, 31254, 2801,
        6864, 10931, 14994, 64814, 60687, 56684, 52557, 48554, 44427, 40424, 36297, 31782,
        27655, 23652, 19525, 15522, 11395, 7392, 3265, 61215, 65342, 53085, 57212, 44955,
        49082, 36825, 40952, 28183, 32310, 20053, 24180, 11923, 16050, 3793, 7920
    ];

    public static ushort ComputeCrc(byte id, ReadOnlySpan<byte> payload)
    {
        int crc = CrcTab[id];
        foreach (byte b in payload)
            crc = CrcTab[((crc >> 8) ^ b) & 0xFF] ^ ((crc << 8) & 0xFFFF);
        return (ushort)(crc & 0xFFFF);
    }

    public static byte[] Encode(MsgId id, params byte[] payload)
    {
        int size = 1 + payload.Length + 2; // id + payload + crc16
        byte[] frame = new byte[4 + size]; // SOM(1) + hdr(2) + size + EOM(1)
        frame[0] = Som;
        frame[1] = (byte)(size & 0xFF);
        frame[2] = (byte)((size >> 8) & 0xFF);
        frame[3] = (byte)id;
        Buffer.BlockCopy(payload, 0, frame, 4, payload.Length);
        ushort crc = ComputeCrc((byte)id, payload);
        frame[4 + payload.Length] = (byte)(crc & 0xFF);
        frame[5 + payload.Length] = (byte)((crc >> 8) & 0xFF);
        frame[6 + payload.Length] = Eom;
        return frame;
    }

    public static byte[] SetNoiseControl(NoiseControlMode mode) => Encode(MsgId.NoiseControls, (byte)mode);
    public static byte[] SetAmbientVolume(int level) => Encode(MsgId.AmbientVolume, (byte)Math.Clamp(level, 0, 4));
    public static byte[] SetAncLevel(int level) => Encode(MsgId.NoiseReductionLevel, (byte)Math.Clamp(level, 0, 4));
    public static byte[] SetVoiceDetect(bool on) => Encode(MsgId.SetDetectConversations, on ? (byte)1 : (byte)0);
    public static byte[] SetVoiceDetectDuration(int idx) => Encode(MsgId.SetDetectConversationsDur, (byte)Math.Clamp(idx, 0, 2));
    public static byte[] SetSirenDetect(bool on) => Encode(MsgId.SirenDetect, on ? (byte)1 : (byte)0);
    public static byte[] SetAmbientDuringCalls(bool on) => Encode(MsgId.SetSidetone, on ? (byte)1 : (byte)0);
    public static byte[] SetOneEarbudNoiseControl(bool on) => Encode(MsgId.SetAncWithOneEarbud, on ? (byte)1 : (byte)0);
    public static byte[] SetCustomizeAmbient(bool on, int left, int right, int tone) =>
        Encode(MsgId.CustomizeAmbientSound,
            on ? (byte)1 : (byte)0,
            (byte)Math.Clamp(left, 0, 2),
            (byte)Math.Clamp(right, 0, 2),
            (byte)Math.Clamp(tone, 0, 4));

    /// <summary>
    /// Extracts all complete valid SPP packets from a rolling receive buffer, leaving incomplete trailing bytes intact.
    /// </summary>
    public static List<SppPacket> ExtractPackets(List<byte> buffer)
    {
        var packets = new List<SppPacket>();
        while (buffer.Count > 0)
        {
            int somIdx = buffer.IndexOf(Som);
            if (somIdx < 0)
            {
                buffer.Clear();
                break;
            }
            if (somIdx > 0)
                buffer.RemoveRange(0, somIdx);

            if (buffer.Count < 7) // Minimum valid packet: SOM + hdr(2) + id(1) + crc(2) + EOM
                break;

            int header = buffer[1] | (buffer[2] << 8);
            int size = header & 0x3FF;
            if (size < 3)
            {
                buffer.RemoveAt(0); // False SOM, resync
                continue;
            }

            int totalLen = 3 + size + 1; // SOM + hdr(2) + (id+payload+crc) + EOM
            if (buffer.Count < totalLen)
                break; // Wait for rest of frame

            int payloadLen = size - 3;
            byte id = buffer[3];
            byte[] payload = new byte[payloadLen];
            for (int i = 0; i < payloadLen; i++)
                payload[i] = buffer[4 + i];

            ushort expectedCrc = ComputeCrc(id, payload);
            byte crcLo = buffer[4 + payloadLen];
            byte crcHi = buffer[5 + payloadLen];
            byte eom = buffer[6 + payloadLen];

            if (eom == Eom && crcLo == (byte)(expectedCrc & 0xFF) && crcHi == (byte)((expectedCrc >> 8) & 0xFF))
            {
                packets.Add(new SppPacket((MsgId)id, payload));
                buffer.RemoveRange(0, totalLen);
            }
            else
            {
                // Bad framing/CRC at this SOM; drop 1 byte and resync to next SOM
                buffer.RemoveAt(0);
            }
        }
        return packets;
    }

    public static bool ApplyPacket(BudsState state, SppPacket pkt)
    {
        var p = pkt.Payload;
        switch (pkt.Id)
        {
            case MsgId.ExtendedStatusUpdated when p.Length >= 34:
                state.BatteryL = p[2];
                state.BatteryR = p[3];
                state.PlacementL = (PlacementState)((p[6] & 0xF0) >> 4);
                state.PlacementR = (PlacementState)(p[6] & 0x0F);
                state.BatteryCase = p[7];
                state.Mode = Enum.IsDefined((NoiseControlMode)p[12]) ? (NoiseControlMode)p[12] : NoiseControlMode.Off;
                state.AmbientVolume = Math.Clamp((int)p[23], 0, 4);
                state.AncLevel = Math.Clamp((int)p[24], 0, 4);
                state.VoiceDetect = p[26] == 1;
                state.VoiceDetectDuration = p[27] <= 2 ? p[27] : 0;
                state.OneEarbudNoiseControl = p[28] == 1;
                state.CustomAmbientOn = p[29] == 1;
                state.CustomAmbientLeft = Math.Clamp((p[30] & 0xF0) >> 4, 0, 2);
                state.CustomAmbientRight = Math.Clamp(p[30] & 0x0F, 0, 2);
                state.CustomAmbientTone = Math.Clamp((int)p[31], 0, 4);
                state.AmbientDuringCalls = p[33] == 1;

                if (p.Length > 42)
                {
                    byte chg = p[42];
                    state.IsLeftCharging = (chg & 16) != 0;
                    state.IsRightCharging = (chg & 4) != 0;
                    state.IsCaseCharging = (chg & 1) != 0;
                    if (state.IsLeftCharging) state.PlacementL = PlacementState.Charging;
                    if (state.IsRightCharging) state.PlacementR = PlacementState.Charging;
                }
                if (p.Length > 52)
                    state.SirenDetect = p[52] == 1;

                state.HasInitialSync = true;
                return true;

            case MsgId.StatusUpdated when p.Length >= 7:
                state.BatteryL = p[1];
                state.BatteryR = p[2];
                state.PlacementL = (PlacementState)((p[5] & 0xF0) >> 4);
                state.PlacementR = (PlacementState)(p[5] & 0x0F);
                state.BatteryCase = p[6];
                if (p.Length > 7)
                {
                    byte chg = p[7];
                    state.IsLeftCharging = (chg & 16) != 0;
                    state.IsRightCharging = (chg & 4) != 0;
                    state.IsCaseCharging = (chg & 1) != 0;
                    if (state.IsLeftCharging) state.PlacementL = PlacementState.Charging;
                    if (state.IsRightCharging) state.PlacementR = PlacementState.Charging;
                }
                return true;

            case MsgId.NoiseControlsUpdate when p.Length >= 1:
                if (Enum.IsDefined((NoiseControlMode)p[0]))
                    state.Mode = (NoiseControlMode)p[0];
                return true;

            case MsgId.AmbientVolume when p.Length >= 1:
                state.AmbientVolume = Math.Clamp((int)p[0], 0, 4);
                return true;

            case MsgId.UniversalAck when p.Length >= 2:
                var ackId = (MsgId)p[0];
                byte val = p[1];
                switch (ackId)
                {
                    case MsgId.NoiseControls when Enum.IsDefined((NoiseControlMode)val):
                        state.Mode = (NoiseControlMode)val;
                        return true;
                    case MsgId.AmbientVolume:
                        state.AmbientVolume = Math.Clamp((int)val, 0, 4);
                        return true;
                    case MsgId.NoiseReductionLevel:
                        state.AncLevel = Math.Clamp((int)val, 0, 4);
                        return true;
                    case MsgId.SetDetectConversations:
                        state.VoiceDetect = val == 1;
                        return true;
                    case MsgId.SetDetectConversationsDur:
                        state.VoiceDetectDuration = Math.Clamp((int)val, 0, 2);
                        return true;
                    case MsgId.SirenDetect:
                        state.SirenDetect = val == 1;
                        return true;
                    case MsgId.SetSidetone:
                        state.AmbientDuringCalls = val == 1;
                        return true;
                    case MsgId.SetAncWithOneEarbud:
                        state.OneEarbudNoiseControl = val == 1;
                        return true;
                    case MsgId.CustomizeAmbientSound when p.Length >= 5:
                        state.CustomAmbientOn = p[1] == 1;
                        state.CustomAmbientLeft = Math.Clamp((int)p[2], 0, 2);
                        state.CustomAmbientRight = Math.Clamp((int)p[3], 0, 2);
                        state.CustomAmbientTone = Math.Clamp((int)p[4], 0, 4);
                        return true;
                }
                return false;

            default:
                return false;
        }
    }

    /// <summary>
    /// Runnable protocol check verified against live Galaxy Buds4 Pro captures.
    /// </summary>
    public static void RunSelfCheck()
    {
        static void Assert(bool cond, string msg) { if (!cond) throw new InvalidOperationException($"Self-check failed: {msg}"); }

        // 1. Live capture from Galaxy Buds4 Pro (EXTENDED_STATUS_UPDATED + ACK concatenated, plus partial chunking)
        byte[] liveExt = Convert.FromHexString(
            "FD410061040D1B64010021000000FF2200006801680103000499000404100000" +
            "00001102000000000000000000000000000000000101000000FF010100000100" +
            "00011029DD");
        byte[] liveAck = Convert.FromHexString("FD055042840415E4DD");

        var buf = new List<byte>();
        buf.AddRange(liveExt[..20]);
        Assert(ExtractPackets(buf).Count == 0, "Partial packet must wait");
        buf.AddRange(liveExt[20..]);
        buf.AddRange(liveAck);
        var pkts = ExtractPackets(buf);
        Assert(pkts.Count == 2 && buf.Count == 0, $"Expected 2 packets, got {pkts.Count}");

        var st = new BudsState();
        Assert(ApplyPacket(st, pkts[0]), "ExtendedStatusUpdated must apply");
        Assert(st.BatteryL == 27 && st.BatteryR == 100, "Battery parse mismatch");
        Assert(st.PlacementL == PlacementState.Idle && st.PlacementR == PlacementState.Wearing, "Placement parse mismatch");
        Assert(st.Mode == NoiseControlMode.Off, "Mode parse mismatch");
        Assert(st.AmbientVolume == 4 && st.AncLevel == 4, "Level parse mismatch");
        Assert(st.CustomAmbientLeft == 1 && st.CustomAmbientRight == 1 && st.CustomAmbientTone == 2, "Custom ambient parse mismatch");

        // 2. Round-trip every outgoing command encoder through ExtractPackets
        var cmds = new[]
        {
            SetNoiseControl(NoiseControlMode.Ambient),
            SetNoiseControl(NoiseControlMode.Adaptive),
            SetAmbientVolume(3),
            SetAncLevel(2),
            SetVoiceDetect(true),
            SetVoiceDetectDuration(1),
            SetSirenDetect(true),
            SetAmbientDuringCalls(true),
            SetOneEarbudNoiseControl(true),
            SetCustomizeAmbient(true, 2, 0, 4)
        };
        foreach (var cmd in cmds)
        {
            var rbuf = new List<byte>(cmd);
            var decoded = ExtractPackets(rbuf);
            Assert(decoded.Count == 1 && rbuf.Count == 0, "Command round-trip failed");
        }

        // 3. Verify ACK application updates state
        var ackFrame = Encode(MsgId.UniversalAck, (byte)MsgId.NoiseControls, (byte)NoiseControlMode.Ambient);
        var ackPkt = ExtractPackets([.. ackFrame])[0];
        Assert(ApplyPacket(st, ackPkt) && st.Mode == NoiseControlMode.Ambient, "NoiseControls ACK failed");

        var sirenAck = Encode(MsgId.UniversalAck, (byte)MsgId.SirenDetect, 1);
        Assert(ApplyPacket(st, ExtractPackets([.. sirenAck])[0]) && st.SirenDetect, "SirenDetect ACK failed");
    }
}
