# Claude Code qua Muse

Adapter thử nghiệm dùng Anthropic Messages API. Model chạy phía sau vẫn là Muse;
đổi tên model không biến nó thành Claude. Không cần tài khoản/API key Anthropic
cho đường gọi inference này; vẫn cần cookie Muse hợp lệ và Claude Code đã cài.

## Chạy

### Claude Desktop trên Windows

1. Cài Claude Desktop. Trong Din Desk, vào **Bắt đầu → Kết nối tài khoản**, thêm
   tài khoản Muse hoặc Gemini Web rồi bấm **Lưu & kết nối**.
2. Khi Din Desk báo **Đã kết nối**, vào **Bắt đầu → Mở Claude Desktop**. Nút này
   lưu profile gateway với key, cổng và model hiện tại rồi mở tab **Code**.
   Nếu Claude Desktop đang dùng cấu hình cũ, thoát hẳn Desktop rồi bấm lại.
   Desktop có thể hỏi xác nhận thư mục dự án.
3. Từ lần sau, mở **Claude Desktop** trực tiếp từ menu Start rồi vào tab **Code**.
   Profile đã được lưu. Giữ Din Desk chạy và kết nối tài khoản trong lúc sử dụng.

Sau khi đổi **API key**, **cổng proxy** hoặc chuyển **Muse ↔ Gemini**, bấm lại
**Mở Claude Desktop** trong Din Desk để cập nhật profile. Nếu Desktop đang chạy,
thoát hẳn rồi mở lại để nhận cấu hình mới.

Model trong giao diện mang tên DinDesk Muse hoặc DinDesk Gemini. Route Desktop trả văn bản và tool call, bỏ qua yêu cầu extended thinking.

### Claude Code CLI

Trong **Din Desk**, kết nối tài khoản rồi bấm **Mở Claude Code** ở tab **Bắt đầu**.
Để dùng từ terminal, bấm **Setup Claude Code** trong **Cài đặt**, mở PowerShell
mới rồi chạy từ thư mục dự án:

```powershell
cy
```

Launcher lấy cổng, model, context và API key hiện tại từ cấu hình DPAPI của app.
Giữ Din Desk chạy khi dùng `cy`. API key của proxy được giữ nguyên khi chuyển
tài khoản. Mở phiên Claude mới sau khi chuyển để dùng đúng model và giữ lịch sử
riêng giữa các tài khoản.

Để tương thích terminal đang giữ hàm `cy` cũ: nếu preflight nhận 401 tại đúng
endpoint loopback do Muse Desk sở hữu, launcher thử lại một lần bằng cấu hình
app. Không áp dụng fallback này cho địa chỉ ngoài máy hoặc port của app khác.

Nếu chạy **server Rust riêng**, dùng cấu hình sau:

Proxy mạng ra ngoài cho Muse có thể cấu hình bằng `MUSE_UPSTREAM_PROXY`, ví dụ
`http://user:password@host:port` hoặc `https://host:port`. Biến này dùng cho cả
bootstrap HTTP và WebSocket/Noise, lỗi không fallback trực tiếp. Không dùng
`MUSE_PROXY_URL` (địa chỉ API localhost của launcher) thay cho biến này. Muse Desk
tự đặt biến từ proxy từng tài khoản; không cần sửa biến môi trường Windows.

Terminal server, tại thư mục dự án:

```powershell
# Giữ cấu hình Muse đã dùng để chat trước đó.
$env:MUSE_COOKIE = '<cookie Muse của bạn>'
$env:MUSE_NODE_ID = '<node ID ổn định của bạn>'
$env:MUSE_PROXY_API_KEY = '<key riêng cho proxy, tự đặt>'

# Chỉ cần hai dòng này nếu terminal vẫn dùng Rustup/Cargo home cũ.
$env:RUSTUP_HOME = Join-Path $env:USERPROFILE '.rustup'
$env:CARGO_HOME = Join-Path $env:USERPROFILE '.cargo'
cargo run
```

Nếu server bản cũ đang chạy, Ctrl+C trong terminal đó rồi chạy lại để tải code và
biến môi trường mới. Không cần đóng một session chat Muse đang mở trên web.

Terminal thứ hai, tại thư mục dự án:

```powershell
$env:MUSE_PROXY_API_KEY = '<cùng key đã đặt ở server>'
.\claude-muse.ps1

# Hoặc gửi một yêu cầu rồi thoát:
.\claude-muse.ps1 -p 'Đọc Cargo.toml rồi giải thích dependencies chính'
```

Chạy từ thư mục dự án khác để Claude Code làm việc ở đó:

```powershell
& 'F:\All Project\muse-api-rust\claude-muse.ps1'
```

Launcher giữ nguyên working directory, kiểm tra endpoint/key trước khi mở Claude
Code, đặt base URL mặc định `http://127.0.0.1:3000` (đổi bằng `MUSE_PROXY_URL`),
pin main/Haiku/Sonnet/Opus/subagent về `muse`, tắt thinking, prompt caching và tool
search, dùng client context budget 256.000 token và output budget 8.192. Launcher khôi
phục các biến môi trường sau khi thoát và không sửa settings/quyền toàn cục của
Claude Code; chỉ loại `WebSearch` tích hợp trong phiên Muse. Với tên model lạ, CLI có thể hiện cảnh báo
`unrecognized_model`; đây không phải bằng chứng request thất bại.

Trước đây window 60.000 kèm output mặc định 32.000 làm input còn quá ít chỗ,
gây compact lặp ở khoảng 32–35 nghìn token. Bản hiện tại dùng 256.000/8.192,
vẫn giữ auto-compact. Thay đổi budget cần mở lại tiến trình `cy`.

Client budget là cấu hình của proxy, **không phải giới hạn native đã xác minh
của Muse cá nhân**. Meta công bố Muse Spark trên Model API/Muse Code có context 1M
([nguồn](https://dev.meta.ai/products/muse-code)); đường Muse cá nhân qua
`muse.ai`/Noise hiện dùng ở đây chưa được xác minh cùng giới hạn. Proxy đã tăng
JSON/context từ 512 KiB lên 4 MiB cùng framing, event, history và cache.
Byte JSON và token là hai giới hạn độc lập. Có thể chọn client budget khác
(64.000–1.000.000) trước khi chạy `cy`, ví dụ
`$env:MUSE_CONTEXT_TOKENS = '500000'`. Giá trị này không mở rộng được giới hạn
upstream, và request vẫn phải nằm trong 4 MiB. Chưa chứng minh đường Muse cá nhân
chạy ổn định ở 256k/500k/1M token native.

Proxy ưu tiên UUID cuộc trò chuyện Claude từ `x-claude-code-session-id` hoặc
`metadata.user_id` (chuỗi `..._session_UUID` và chuỗi JSON chứa `session_id`).
Nếu cả header và metadata đều có ID thì phải khớp. Thiếu ID native mới dùng
`x-muse-client-id` do launcher tạo; thiếu cả hai thì xử lý stateless. Những ID
này chỉ phân vùng cache, không phải API key. Không dùng account/user ID làm
session. ID không được ghi vào log. Key proxy có thể đọc từ biến
môi trường User nếu terminal hiện tại chưa có `MUSE_PROXY_API_KEY`.

## Tìm kiếm web

Launcher tự nối công cụ `mcp__muse-web-search__web_search_exa` qua MCP HTTP của
Exa và loại `WebSearch` tích hợp khỏi phiên Muse. `WebSearch` tích hợp gửi request
server tool của Anthropic, proxy Muse không thực thi được loại request đó.
`WebFetch` đọc URL cụ thể vẫn giữ nguyên.

Không cần API key Exa để bắt đầu; dịch vụ ẩn danh có giới hạn tần suất. Chỉ truy
vấn tìm kiếm được gửi sang Exa khi gọi công cụ, không gửi cookie Muse hay key proxy.
Cấu hình MCP là file tạm cho từng phiên, được xóa khi launcher kết thúc. Không sửa
cấu hình Claude toàn cục hoặc loại bỏ MCP khác đang dùng. Cơ chế hỏi quyền công cụ
của Claude vẫn áp dụng như trước.

Sau khi cập nhật launcher, thoát Claude rồi mở lại bằng `cy` hoặc nút “Mở Claude
Code” trong Muse Desk. Dùng `cy --resume` nếu muốn tiếp tục hội thoại cũ. Phiên
Claude đang mở chưa có MCP mới cho tới khi khởi chạy lại.

Đã kiểm chứng thật trên máy phát triển: Claude Code dùng model `muse`, MCP
`muse-web-search` kết nối thành công, `WebSearch` tích hợp không còn trong danh
sách tool, gọi `web_search_exa` nhận kết quả và trả hai URL tài liệu Tokio thật.
14 kiểm tra desktop qua, gồm launcher trên Windows PowerShell, giữ tham số
`--resume`/working directory và xóa cấu hình MCP tạm sau khi thoát.

Nguồn: https://github.com/exa-labs/exa-mcp-server và
https://code.claude.com/docs/en/cli-reference.

## Phần đã hỗ trợ

- `POST /v1/messages`: JSON và SSE Anthropic, kể cả URL có `?beta=true`.
- `POST /v1/messages/count_tokens`: đếm **ước lượng** cho compatibility.
- Auth bằng `Authorization: Bearer <MUSE_PROXY_API_KEY>` hoặc `x-api-key`.
  Thiếu key cấu hình trả 503, key sai trả 401.
- Giữ đầy đủ system prompt, cả system messages xen giữa hội thoại của client mới,
  lịch sử assistant/user, tool_use và tool_result (kể cả is_error).
- Nhận ảnh base64 trong user content và tool_result, kể cả ảnh do Claude Code
  `Read` trả về. PNG/JPEG/GIF/WebP, tối đa 2 MiB sau giải mã base64 mỗi ảnh và
  128 ảnh khác nhau trong toàn bộ lịch sử; tổng JSON request vẫn phải nằm trong 4 MiB.
  Giới hạn này áp dụng cho cả chat, đếm token và request tóm tắt của `/compact`.
  Đã kiểm tra API thật với 10 ảnh nằm ở các lượt cũ: đếm token và yêu cầu tóm
  tắt đều trả 200, tóm tắt nhận đủ 10 ảnh. Không tự bỏ ảnh cũ khỏi context.
- Client tools với JSON Schema, tool_choice auto/none/any/named và giới hạn gọi
  song song. Tên tool lạ, input sai schema, kết quả orphan/trùng ID hoặc JSON
  upstream lỗi đều bị từ chối; không phát một phần batch tool rồi mới báo lỗi.

Tool chạy trên máy do **Claude Code** thực thi theo quyền của nó. Proxy không
chạy shell hay ghi file thay Claude Code. Các tool Muse nội bộ không được ánh xạ
thành tool trên máy của bạn.

Ảnh được gửi thành item ảnh thực tới Muse, cùng label `image_id`, không nhét
base64 vào prompt văn bản. Transcript lưu reference SHA-256 ổn định để giữ thứ
tự/ngữ cảnh; prefix cache chỉ gửi các ảnh thuộc message mới ở lượt tiếp theo.
Ảnh cùng nội dung được deduplicate trong request. Khi context phải tạo lại,
ảnh trong lịch sử caller được gửi lại để giữ đủ thông tin. Dữ liệu ảnh chỉ nằm
trong RAM của proxy; không ghi file hoặc log. Muse xử lý upload theo dịch vụ của nó.

Ví dụ image block (dùng được trong `user.content` hoặc `tool_result.content`):

```json
{"type":"image","source":{"type":"base64","media_type":"image/png","data":"<base64 PNG>"}}
```

Chỉ hỗ trợ nguồn base64; URL/file_id chưa được resolve. Base64 sai, MIME không
hỗ trợ/không khớp magic bytes và ảnh vượt giới hạn trả 400 cụ thể. Document/PDF
và audio vẫn chưa hỗ trợ.

## Cách hoạt động và giới hạn

Đây là **prompt-based tool bridge**, chưa phải native tool calling từ Muse.
Lượt đầu gửi system/tools/toàn bộ transcript vào một session Muse riêng. Với
ID phiên native của Claude (hoặc `x-muse-client-id` dự phòng), lượt tiếp theo dùng lại session/socket **chỉ khi** system và
tool definitions không đổi, lịch sử mới bắt đầu chính xác bằng transcript đã
hoàn tất. Proxy gửi nonce/controls mới, các message vừa thêm và assistant turn
đã xác nhận để đồng bộ tool IDs; không gửi lại system/tools/lịch sử cũ sang Muse.
Claude Code vẫn gửi đầy đủ request qua localhost để proxy kiểm tra prefix.

Đóng/mở lại launcher rồi resume cùng conversation có thể dùng tiếp cache nếu
proxy vẫn chạy, entry chưa hết TTL/bị loại, system/tools và prefix vẫn khớp.
Restart proxy làm mất cache trong RAM; ID native không khôi phục được context
đã mất. Request phụ/compact có cùng ID vẫn bị phân nhánh khi lịch sử/chỉ dẫn
khác; chưa nhận diện riêng tên tác vụ phụ. Không suy diễn session theo đoạn
text gần giống.

Đã kiểm tra Claude Code cài trên máy gửi cùng session UUID ở cả header và
metadata tới endpoint giả lập. Kiểm tra HTTP với Muse thật: hai request cùng
ID native nhưng khác ID launcher dùng `full` rồi `delta`, phản hồi thứ hai
nhớ marker từ lượt trước. Kiểm thử hồi quy còn kiểm tra chat khác, lịch sử
compact và ID xung đột. Chưa có lưu cache transcript xuống đĩa.

Sửa lịch sử, đổi system/tools, compaction, nhánh khác hoặc lượt trước lỗi/hủy
sẽ dùng context mới. Request đồng thời không ghi đè session đang xử lý. Không
có header nhận diện hoặc backend không hỗ trợ session thì giữ chế độ stateless.
Cache tối đa 16 prefix; mỗi entry tối đa 4 MiB JSON system/tools/history đã
serialize (bộ nhớ thực tế có thêm overhead). Entry chỉ được tái dùng trong 30
phút kể từ lần dùng cuối; hết hạn được dọn khi có truy cập, hoặc bị LRU evict.
Prompts/prefix chỉ nằm trong RAM, mất khi restart; không ghi xuống file. Session
inference không ghi vào `/v1/conversations`; upstream vẫn tạo session persistent,
chưa có cơ chế dọn chat đó trên Muse.

Log `Muse agent context`/`Muse agent inference finished` ghi `mode=full|delta`,
`upstream_bytes`, `elapsed_ms`; không ghi nội dung prompt. Cảnh báo prompt caching
off của Claude Code nói về Anthropic prompt caching, không phải cache delta này.

SSE phát `message_start` và keep-alive khi chờ, nhưng nội dung/text/tool input
được **đệm tới khi cả lượt Muse hoàn tất và JSON được kiểm tra**. Các text delta
sau đó là chia chunk tương thích giao thức, không phải token streaming realtime.
Lỗi/hủy không phát `message_stop` thành công. Ngắt client dừng chờ/đóng socket
cục bộ, chưa gửi cancel task lên Muse.

Request/context tối đa 4 MiB; quá giới hạn trả lỗi, không lặng lẽ cắt lịch sử.
History/unary/NDJSON tối đa 32 MiB, tổng dữ liệu transport đang ghép tối đa
64 MiB và 2.048 chunk; mỗi gói Noise vẫn tối đa 65.535 byte như giao thức gốc.
Reply tối đa 2 MiB; cả inference tối đa 180 giây (tạo session tối đa 40 giây,
run giữ deadline 120 giây hiện có). `max_tokens` chỉ là mục tiêu trong prompt,
không phải giới hạn tokenizer upstream được bảo đảm. `temperature`, `top_p`,
effort và metadata không điều khiển model Muse. Usage/count_tokens được tính
bằng `ceil(số ký tự context/4) + 2048 * số ảnh khác nhau`, không tính base64 như
text; budget ảnh là ước lượng compatibility, không phải tokenizer/usage native.
Output dựa trên JSON content trả về. Header
`x-muse-usage: estimated-text-and-images` ghi rõ nguồn ước lượng. Chi phí USD mà Claude
Code tự tính cho model lạ không phản ánh hóa đơn Muse.

Document/audio, extended thinking, server tools, deferred tool search,
structured output, assistant prefill, custom stop và server context management
chưa hỗ trợ; các yêu cầu đó trả 400. Schema chỉ được tham chiếu local `#`, không
fetch schema từ mạng hoặc filesystem.

Din Desk yêu cầu API key cho các route API localhost, gồm Anthropic Messages,
Responses, Chat Completions và conversation. Xem [quản lý key và ngày hết hạn](WINDOWS_RELEASE.md#api-key-và-ngày-hết-hạn).

## Kiểm chứng

Vision đã thử với Muse thật bằng PNG có mã sáu chữ số ngẫu nhiên và hình tròn
màu xanh, không đưa đáp án trong prompt: user image trả đúng cả mã/màu; câu hỏi
tiếp theo cùng session không tải lại ảnh vẫn đúng. Claude Code 2.1.240 dùng tool
`Read` mở file PNG, gửi image trong tool_result, rồi trả đúng mã/màu sau hai lượt.
Unit/HTTP tests kiểm tra cả hai vị trí image block, giữ binary ngoài prompt,
prefix delta, MIME/base64/size và backend visual dispatch. Kiểm chứng này không
đánh giá độ chính xác thị giác trên mọi loại ảnh.

Đã chạy Claude Code 2.1.240 thực tế với fixture trong thư mục tạm:

1. Model yêu cầu tool `Read` đọc file chứa UUID không có trong prompt.
2. Claude Code thực thi Read và gửi kết quả về.
3. Model yêu cầu `Write` ghi UUID sang file khác.
4. Claude Code ghi đúng UUID, gửi tool_result, nhận câu kết thúc.

Ca này đã pass cả backend giả lập và **Muse thật**, qua gateway Anthropic thử
nghiệm nối tới `/v1/chat/completions` của server Muse đang chạy. Đây là kiểm chứng
vòng tool cơ bản, không phải benchmark khả năng sửa một repository lớn. Các
unit/HTTP test riêng kiểm tra schema, auth, cancellation, SSE và history.

Đã bổ sung ca chạy **trực tiếp gateway production + Muse thật với toàn bộ tool
mặc định**: ba request Read → Write → end_turn khoảng 88–90 KiB mỗi request,
được chia thành nhiều protobuf transport frame trước khi mã hóa Noise. Mỗi gói
Noise vẫn giữ giới hạn 65.535 byte; không tăng giới hạn giao thức mã hóa. Echo
user từ Muse chỉ giữ ID phục vụ correlation, tránh lưu lặp prompt lớn vào hàng
chờ trước ACK. Đây là bản sửa lỗi 502 `Noise plaintext is too large` gặp khi dùng
Claude Code với bộ tool đầy đủ. Có regression ở ranh giới chunk, request lớn
hơn 4 MiB qua Noise thật và user echo lớn trước ACK. HTTP test nhận JSON 1 MiB
qua endpoint thật và từ chối vượt 4 MiB bằng error 413; NDJSON test nhận echo 6 MiB.

Sau khi nâng giới hạn, Muse thật trả đúng ba marker ở đầu/giữa/cuối request
324.044 byte, tương đương 81.024 token theo bộ ước lượng ký tự của proxy. Hai ca
lớn hơn (647.680 byte / 161.933 ước lượng và 971.317 byte / 242.842 ước lượng)
đã qua lớp HTTP/framing nhưng Muse báo task failed. Không suy ra nguyên nhân
chính xác hoặc giới hạn native chỉ từ lỗi đó; không coi client budget 256k là
bằng chứng upstream hỗ trợ đủ 256k. Proxy không tự cắt nội dung hay giả thành công.

Sau tối ưu đã chạy lại Read → Write → end_turn bằng Claude Code và Muse thật:
request đầu 84.682 byte / 11.341 ms; hai request tiếp theo 1.486 byte / 4.434 ms
và 1.684 byte / 3.139 ms. Cùng session/socket, file đầu ra khớp UUID đọc được.
Đây là số đo ba lượt trong một task, không phải benchmark các prompt giống nhau
hay bảo đảm độ trễ cho mọi tác vụ. Lượt đầu vẫn tải đủ context. Token/chi phí
upstream thực không được Muse cung cấp; không suy ra tiết kiệm token hay tiền
từ mức giảm byte gửi. Tests bao phủ prefix, thay context/nhánh/client,
error/cancel, TTL và giới hạn cache.

Chạy lại kiểm chứng bằng backend giả lập (không gọi Muse):

```powershell
$env:MUSE_TEST_CLAUDE_EXE = 'C:\Program Files\nodejs\node_modules\@anthropic-ai\claude-code\bin\claude.exe'
cargo test --lib claude_code_reads_and_writes_a_fixture_through_real_tools -- --ignored --nocapture
```

Để dùng Muse thật, thêm `$env:MUSE_TEST_UPSTREAM_URL = 'http://127.0.0.1:3000'`
trước lệnh test. Server Muse phải đang chạy/có cookie. Test chỉ bật Read/Write và
yêu cầu thao tác trên fixture; launcher dùng hằng ngày không tự thêm quyền này. Bỏ
`MUSE_TEST_UPSTREAM_URL` để quay lại backend giả lập.

Kiểm thử trực tiếp đường production với bộ tool đầy đủ (server đã chạy):

```powershell
Remove-Item Env:MUSE_TEST_UPSTREAM_URL -ErrorAction SilentlyContinue
$env:MUSE_TEST_GATEWAY_URL = 'http://127.0.0.1:3000'
$env:MUSE_TEST_ALL_TOOLS = '1'
$env:MUSE_TEST_LAUNCHER = '1'
$env:MUSE_PROXY_API_KEY = [Environment]::GetEnvironmentVariable('MUSE_PROXY_API_KEY', 'User')
cargo test --lib claude_code_reads_and_writes_a_fixture_through_real_tools -- --ignored --nocapture
```

Đặt `MUSE_TEST_CLAUDE_EXE` như ví dụ bên trên. Các tool được gửi đầy đủ trong ca
này nhưng chỉ tự cho phép Read/Write; nhiệm vụ vẫn chỉ đọc/ghi fixture. Bỏ các
biến `MUSE_TEST_GATEWAY_URL`, `MUSE_TEST_ALL_TOOLS`, `MUSE_TEST_LAUNCHER` sau khi
chạy để quay lại cấu hình test mặc định.

Tham khảo: [Claude Code gateway](https://code.claude.com/docs/en/llm-gateway-connect),
[Messages streaming](https://platform.claude.com/docs/en/build-with-claude/streaming),
[tool use](https://platform.claude.com/docs/en/agents-and-tools/tool-use/overview),
[environment variables](https://code.claude.com/docs/en/env-vars).
# Khôi phục lỗi Muse task

Nếu Muse báo task thất bại khi xử lý context lớn hơn 256 KiB, adapter thử
khôi phục đúng một lần trong session mới. Prompt đầy đủ được nạp theo thứ tự,
mỗi phần tối đa 240 KiB và phải được Muse xác nhận chính xác trước khi gửi
phần kế tiếp. Sau cùng mới yêu cầu phản hồi và kiểm tra toàn bộ JSON/tool schema.
Fragment được bọc thành chuỗi JSON để tách lịch sử khỏi chỉ dẫn của lượt upload;
chỉ dẫn xác nhận nằm cuối tin nhắn. Giới hạn phần tính cả ký tự escape của JSON.
Receipt chấp nhận ACK có bọc code fence, JSON string hoặc object chỉ có trường
`ack`, nhưng nonce và số thứ tự vẫn phải khớp. Nếu phản hồi hoàn tất sai định dạng,
proxy hỏi xác nhận lại đúng một lần, không gửi lại fragment/ảnh. Không chuyển
phản hồi tool thành xác nhận hợp lệ.

Nếu vẫn không xác nhận được, JSON trả HTTP 400 / `invalid_request_error`, SSE trả
error cùng loại, không trả tool hay message_stop thành công. Đây là lỗi chuyển
context phía proxy/upstream, không phải khẳng định request của client sai cú pháp.
Lưu SHA-256 request thất bại trong RAM tối đa 16 mục / 60 giây, theo client/session;
fallback stream → JSON cùng nội dung nhận lại lỗi ngay thay vì upload lại. Nonce
mới không né được chặn này; đổi nội dung hoặc hết 60 giây thì có thể thử lại.
Không lưu prompt/ảnh trong danh sách lỗi. Claude Code cài trên máy đã được thử
với gateway giả: một stream rồi hai JSON fallback, dừng ngay trên lỗi 400 thay
vì lặp backoff 502. Chưa chứng minh bằng nguyên văn phiên coding đã gặp lỗi.
Từ beta.4, khi khôi phục, toàn bộ ảnh của request được gửi trong một batch,
kèm manifest image_id/vị trí và một ACK chung. Nếu nhận HTTP 413 hoặc Muse báo
task failed rõ ràng, batch được chia đôi, tiếp tục chia riêng phần lỗi đến tối
thiểu một ảnh. Batch đã ACK không bị gửi lại. Không chia/gửi lại khi timeout,
mất kết nối, cancel hoặc receipt không xác nhận được. Giới hạn 90 giây cho pha
ảnh, tối đa 2N-1 lần thử batch với N ảnh; lỗi ở một ảnh đơn thì dừng. Lượt commit
cuối không gửi lại ảnh. Chunk text 240 KiB vẫn là cơ chế riêng, chưa đổi ở beta.4.

Task failed không tự chứng minh lỗi do kích thước. Lượt cha thất bại có thể vẫn
nằm trong history upstream; image_id giúp phân biệt ảnh trùng về ý nghĩa nhưng
không bảo đảm giảm bộ nhớ/context native. Chia batch không tăng context window.
Không cắt hoặc tóm tắt lịch sử. Không dùng cách khôi phục này cho timeout,
mất kết nối, hủy yêu cầu hoặc phản hồi tool không hợp lệ. Việc nạp lại có thể
làm lượt khôi phục chậm hơn. Đây không phải cách tăng giới hạn context native
của Muse và không bảo đảm khắc phục mọi lỗi task phía Muse.

Đã kiểm tra nạp khoảng 580 KB qua ba lượt xác nhận với Muse thật; phản hồi cuối
nhận được cả marker ở đầu và cuối context. Kiểm tra này xác nhận cơ chế nạp,
chưa tái hiện nguyên văn yêu cầu coding đã thất bại của người dùng.

Bản sửa receipt đã chạy trực tiếp `infer_uploaded` với Muse thật: 631.138 byte,
5 fragment (có JSON escape và Unicode), nhận được cả hai marker ở đầu/cuối sau
57,55 giây. Context thử có cả chỉ dẫn output JSON/không trả ACK trong lịch sử.
Đây là fixture kiểm tra cơ chế mới, không phải toàn bộ transcript coding đang lỗi.
Test `muse_live_staged_coding_context_preserves_markers` mặc định bỏ qua vì gọi
Muse thật và cần cookie. `start-fixed-proxy.ps1 -VerifyContextUpload` chạy test này
trước khi thay server; nếu test không qua thì giữ server cũ.

`start-fixed-proxy.ps1` chạy bản release đã build: nhập cookie cục bộ qua ô
che nội dung, kiểm tra tài khoản ở cổng thử nghiệm trước khi thay server cũ
của dự án trên cổng 3000. Cookie không được ghi vào file.

## Khôi phục phản hồi sai JSON

Backend beta.2 sửa riêng lỗi `Muse did not return the agent JSON protocol`:
chỉ sau khi Muse báo hoàn tất, nếu phản hồi không parse được JSON thì yêu cầu
trả lại đúng định dạng một lần trong cùng conversation, tối đa 45 giây. Lượt sửa
không gửi lại transcript, ảnh hay tool_result, không mở conversation mới. Vẫn
kiểm tra toàn bộ nonce, tool đã khai báo, schema và tool_choice trước khi trả
bất kỳ tool nào. JSON hợp lệ nhưng sai nonce/schema không được sửa tự động;
cancel, mất kết nối và stream chưa hoàn tất cũng không kích hoạt sửa định dạng.

Nếu vẫn không hợp lệ, JSON trả 400 và SSE trả `invalid_request_error`. Cùng
request/client bị chặn gọi upstream lại trong 60 giây, kể cả fallback stream
sang JSON với nonce mới. Cache lỗi dùng digest và có giới hạn 16 mục; không lưu
prompt hay ảnh. Đây là lỗi định dạng upstream, không khẳng định client sai cú pháp.
Log chỉ ghi độ dài và vị trí/loại lỗi parse, không ghi nội dung phản hồi.

Kiểm chứng ngày 13/11/2026: 74 regression test qua; Muse thật trả lại JSON hợp lệ
sau một lượt văn bản thường trong cùng phiên; request dựng lại từ nhánh Tasks
sau 6 Read (89.887 byte) trả 200, các tool trả về không được thực thi trong replay.
Claude Code thật chạy hai subagent song song trên fixture riêng: mỗi nhánh Read
6 file rồi Write kết quả, cả hai đầu ra khớp đủ marker riêng; hoàn tất 59,08 giây.
Đây không phải kiểm chứng toàn bộ task DinCRM với năm nhánh và lịch sử 205k token.

## Kiểm chứng upload ảnh beta.4

Đã thử Muse thật với 12 ảnh chứa nhãn ngẫu nhiên: một batch 12 ảnh, một ACK nhóm,
commit đọc đúng cả 12 nhãn theo thứ tự, hoàn tất 22,73 giây gồm nạp text, ảnh và
inference cuối. Không thực thi client tool trong probe. Unit test kiểm tra cả
hai loại lỗi task failed/413, chia lẻ 11→5+6→2+3+3+3, giữ phần đã thành công,
dừng khi một ảnh lỗi hoặc receipt/transport không chắc chắn, timeout và số lượt
tối đa. Backend giữ được thông tin 413 từ Noise stream tới bộ phân loại lỗi.
Probe thứ hai dùng adapter test cố ý từ chối batch lớn hơn 3 ảnh trước khi gửi
tới Muse: chuỗi thử 12,6,3,3,6,3,3; bốn batch 3 ảnh thực gửi tới Muse và commit
vẫn đọc đúng 12 nhãn theo thứ tự. Đây là lỗi 413 được tiêm để kiểm tra chia đôi,
không phải bằng chứng Muse có giới hạn 3 ảnh. 88 regression test và 10 self-test
app qua; các probe không sửa file hay thực thi client tool.

