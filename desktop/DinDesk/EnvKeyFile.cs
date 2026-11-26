using System.Security.AccessControl;
using System.Text;
using System.Text.RegularExpressions;

namespace MuseDesk;

internal static class EnvKeyFile
{
    internal const string DefaultVariable = "MUSE_PROXY_API_KEY";
    private static readonly Regex Variable = new("\\A[A-Za-z_][A-Za-z0-9_]*\\z");
    private static readonly Regex Assignment = new(@"\G[ \t]*(?:export[ \t]+)?(?<name>[A-Za-z0-9_.-]+)[ \t]*=");
    private static readonly Encoding[] BomEncodings = [
        new UTF32Encoding(false, true, true), new UTF32Encoding(true, true, true),
        new UTF8Encoding(true, true), new UnicodeEncoding(false, true, true), new UnicodeEncoding(true, true, true)
    ];

    internal static bool IsVariableName(string? name) => name != null && Variable.IsMatch(name);

    internal static void Update(Settings settings)
    {
        settings.RequireValidApiKey();
        string path = settings.EnvKeyFilePath ?? throw new ArgumentException("Chọn file .env trước khi cập nhật key.");
        string variable = settings.EnvKeyVariable;
        if (!IsVariableName(variable)) throw new ArgumentException("Tên biến .env không hợp lệ.");
        string key = settings.ApiKey;
        if (key.Length == 0 || !key.All(character => char.IsAsciiLetterOrDigit(character) || character is '_' or '-' or '.'))
            throw new ArgumentException("API key hiện tại có ký tự không phù hợp với file .env. Đổi API key trước khi cập nhật.");

        try { ReplaceValue(path, variable, key); }
        catch (Exception error) when (error is FileNotFoundException or DirectoryNotFoundException)
        {
            throw new InvalidOperationException("Không tìm thấy file .env đã chọn. Hãy chọn lại file.");
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            throw new InvalidOperationException("Không ghi được file .env. Kiểm tra quyền ghi hoặc ứng dụng đang giữ file rồi thử lại.");
        }
        catch (DecoderFallbackException)
        {
            throw new InvalidOperationException("Không đọc được mã hóa văn bản của file .env.");
        }
    }

    private static void ReplaceValue(string path, string variable, string key)
    {
        byte[] original = File.ReadAllBytes(path);
        // An ASCII-compatible file without a BOM round-trips byte-for-byte through Latin-1.
        Encoding encoding = BomEncodings.FirstOrDefault(candidate => original.AsSpan().StartsWith(candidate.Preamble)) ?? Encoding.Latin1;
        string text = encoding.GetString(original.AsSpan(encoding.Preamble.Length));
        if (text.Contains('\0')) throw new InvalidOperationException("File đã chọn cần là file .env dạng văn bản.");

        var (start, end, spaceBeforeComment) = FindValue(text, variable);
        string value = spaceBeforeComment ? key + " " : key;
        if (text.AsSpan(start, end - start).SequenceEqual(value)) return;
        string updated = string.Concat(text.AsSpan(0, start), value, text.AsSpan(end));

        string staging = Path.Combine(Path.GetDirectoryName(path)!, $".dindesk-{Guid.NewGuid():N}");
        string temporary = staging + ".tmp";
        string previous = staging + ".bak";
        bool committed = false;
        try
        {
            if ((File.GetAttributes(path) & FileAttributes.ReadOnly) != 0) throw new UnauthorizedAccessException();
            var access = new FileInfo(path).GetAccessControl(AccessControlSections.Access);
            using (var output = new FileInfo(temporary).Create(FileMode.CreateNew, FileSystemRights.Write,
                FileShare.None, 4096, FileOptions.None, access))
            {
                output.Write(encoding.Preamble);
                output.Write(encoding.GetBytes(updated));
                output.Flush(flushToDisk: true);
            }
            // Keep ordinary writers out between the comparison and the atomic replacement.
            using var current = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read | FileShare.Delete);
            if (current.Length != original.Length || !FileMatches(current, original))
                throw new InvalidOperationException("File .env vừa được sửa. Kiểm tra nội dung rồi bấm cập nhật lại.");
            File.Replace(temporary, path, previous);
            committed = true;
        }
        catch (IOException) when (File.Exists(previous) && !File.Exists(path))
        {
            // Windows can rename the original before reporting a replacement failure.
            File.Move(previous, path);
            throw;
        }
        finally
        {
            if (File.Exists(temporary)) File.Delete(temporary);
            if (committed) File.Delete(previous);
        }
    }

    private static bool FileMatches(FileStream file, byte[] expected)
    {
        byte[] current = new byte[expected.Length];
        file.ReadExactly(current);
        return current.AsSpan().SequenceEqual(expected);
    }

    private static (int Start, int End, bool SpaceBeforeComment) FindValue(string text, string variable)
    {
        (int Start, int End, bool SpaceBeforeComment)? found = null;
        for (int line = 0; line < text.Length;)
        {
            int end = EndOfLine(text, line);
            Match assignment = Assignment.Match(text, line, end - line);
            line = NextLine(text, end);
            if (!assignment.Success) continue;

            int afterEquals = assignment.Index + assignment.Length;
            int start = SkipSpace(text, afterEquals, end);
            bool isTarget = assignment.Groups["name"].Value == variable;
            if (isTarget && found.HasValue)
                throw new InvalidOperationException($"Biến {variable} xuất hiện nhiều lần trong file .env.");

            if (start < end && text[start] is '\'' or '"' or '`')
            {
                int closing = ClosingQuote(text, start);
                end = EndOfLine(text, closing);
                line = NextLine(text, end);
                if (!isTarget) continue;
                int suffix = SkipSpace(text, closing + 1, end);
                if (suffix != end && text[suffix] != '#')
                    throw new InvalidOperationException($"Giá trị của biến {variable} có ký tự thừa sau dấu nháy.");
                found = (start + 1, closing, false);
            }
            else if (isTarget)
            {
                int comment = text.IndexOf('#', start, end - start);
                if (comment == start)
                {
                    // Retain the original spaces before the comment on an empty assignment.
                    found = (afterEquals, afterEquals, afterEquals == comment);
                    continue;
                }
                int valueEnd = comment < 0 ? end : comment;
                while (valueEnd > start && text[valueEnd - 1] is ' ' or '\t') valueEnd--;
                found = (start, valueEnd, false);
            }
        }
        return found ?? throw new InvalidOperationException($"Không tìm thấy biến {variable} trong file .env.");
    }

    private static int ClosingQuote(string text, int start)
    {
        char quote = text[start];
        for (int index = start + 1; index < text.Length; index++)
        {
            if (text[index] == quote) return index;
            if (text[index] == '\\' && index + 1 < text.Length && (text[index + 1] == quote || text[index + 1] == '\\')) index++;
        }
        throw new InvalidOperationException("File .env có giá trị thiếu dấu nháy đóng.");
    }

    private static int EndOfLine(string text, int start)
    {
        int offset = text.AsSpan(start).IndexOfAny('\r', '\n');
        return offset < 0 ? text.Length : start + offset;
    }

    private static int NextLine(string text, int end) => end < text.Length && text[end] == '\r'
        && end + 1 < text.Length && text[end + 1] == '\n' ? end + 2 : end + 1;

    private static int SkipSpace(string text, int start, int end)
    {
        while (start < end && text[start] is ' ' or '\t') start++;
        return start;
    }
}
