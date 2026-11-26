using System.Text.Json;
using System.Text.Json.Nodes;

namespace MuseDesk;

internal static class ClaudeDesktopSetup
{
    private const string ProfileId = "f697f54d-199e-4f89-a64a-989dc831f556";

    internal static bool Configure(Settings settings, string local, string roaming)
    {
        settings.RequireValidApiKey();
        string library = Path.Combine(local, "Claude-3p", "configLibrary");
        string metaPath = Path.Combine(library, "_meta.json");
        string configPath = Path.Combine(roaming, "Claude", "claude_desktop_config.json");
        string profilePath = Path.Combine(library, ProfileId + ".json");
        var meta = Read(metaPath);
        var config = Read(configPath);
        var profile = Read(profilePath);
        var entries = meta["entries"]?.AsArray() ?? new JsonArray();
        if (meta["entries"] == null) meta["entries"] = entries;
        if (!entries.Any(entry => (string?)entry?["id"] == ProfileId))
            entries.Add(new JsonObject { ["id"] = ProfileId, ["name"] = "DinDesk" });
        meta["appliedId"] = ProfileId;
        config["deploymentMode"] = "3p";
        profile["inferenceProvider"] = "gateway";
        profile["inferenceGatewayBaseUrl"] = $"http://127.0.0.1:{settings.Port}";
        profile["inferenceGatewayAuthScheme"] = "bearer";
        profile["inferenceGatewayApiKey"] = settings.ApiKey;
        profile["inferenceModels"] = new JsonArray(new JsonObject {
            ["name"] = "claude-dindesk-" + settings.ModelId,
            ["labelOverride"] = settings.Provider == "gemini" ? "DinDesk Gemini" : "DinDesk Muse"
        });
        // Write the profile before selecting it. A retry can finish an interrupted setup.
        bool changed = Save(profilePath, profile);
        changed |= Save(metaPath, meta);
        changed |= Save(configPath, config);
        return changed;
    }

    private static JsonObject Read(string path) =>
        File.Exists(path) ? (JsonNode.Parse(File.ReadAllText(path))?.AsObject()
            ?? throw new InvalidOperationException("Cấu hình Claude Desktop trống.")) : new JsonObject();

    private static bool Save(string path, JsonObject value)
    {
        if (File.Exists(path) && JsonNode.DeepEquals(Read(path), value)) return false;
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        string temp = path + "." + Guid.NewGuid().ToString("N") + ".tmp";
        try
        {
            File.WriteAllText(temp, value.ToJsonString(new JsonSerializerOptions { WriteIndented = true }));
            File.Move(temp, path, true);
            return true;
        }
        finally { File.Delete(temp); }
    }
}
