# Muse Desk cho Windows

**Trạng thái hiện tại:** giao diện đang được chỉnh và xem trước, chưa chốt bản
phân phối. `target/desktop-preview-accounts/MuseDesk.exe` là bản UI mới; ZIP beta dưới
đây chứa giao diện trước đó. Chỉ build lại ZIP sau khi hoàn tất phần xem trước.

EXE xem trước hiện là **0.1.0-beta.18**, đã nhúng backend có sửa ACK, khôi phục
JSON một lần trong cùng phiên và chặn retry request lỗi trong 60 giây. Runtime
được giải nén theo phiên bản; cấu hình tài khoản DPAPI vẫn dùng thư mục cũ.
Self-test app kiểm tra backend Muse/Gemini, proxy từng acc, server nhúng, quota và launcher Claude Code đã cài.

Beta.18 thêm model opt-in `gemini-decision` trên `/v1/messages`. Luồng này gọi
Gemini Decision 1 để chọn đúng một hành động, sau đó Gemini 3.5 Flash-Lite sinh
tham số hoặc câu trả lời; **không dùng Muse để suy luận**. Chỉ xuất hiện trong
`/v1/models` khi có `DECISION_API_KEY`. Muse Desk đọc một key từ kho DPAPI riêng
`decision/gemini-existing-keys.protected` dưới thư mục dữ liệu của app; không xoay
key/project khi hết quota. Người chạy Rust trực tiếp có thể đặt `DECISION_API_KEY`
trong môi trường. Key không nằm trên command line hoặc trong release.

Model mới nhận văn bản tối đa 512 KiB kể cả schemas, chưa nhận ảnh và chỉ đề xuất
một tool mỗi lượt. Mỗi tool phải có tên đã khai báo và input qua schema gốc;
phản hồi chưa hoàn tất hoặc hết quota đều không phát lệnh tool. Khi xác suất chọn
thấp, Flash lập một bước rồi vẫn qua các kiểm tra schema và giới hạn một tool.
Bộ chọn chỉ nhận tên/mô tả tool; schema tham số đầy đủ dùng ở bước sinh/kiểm tra.
Rakazo vẫn thực thi tool dưới quyền của nó. Cần khai báo connection là
`anthropic-messages`, context tối đa 120.000 tokens, output 8.192 và không nhận ảnh.
Hai API dùng chung một project free tier; không bật billing tự động. Dữ liệu hội
thoại gửi cho Google theo điều khoản free tier. Model `muse` và launcher Claude
giữ đường cũ; đổi sang `gemini-decision` là lựa chọn sử dụng Google API rõ ràng.

Kiểm chứng trên bản đang chạy: Nova chào trong nhóm; Mika dùng `web_search` và
`web_fetch` trong lượt thử đầu. Lượt nghiên cứu dài ban đầu gặp 429 và một báo cáo
thiếu nguồn, nên chưa coi đó là kết quả nghiên cứu hợp lệ. Sau khi giảm dữ liệu
cho router và làm rõ phạm vi tác vụ, bài thử tập trung gọi `web_fetch` đúng một
lần, trả URL cùng hai quan sát và không nhắn bot khác. Nova đã nhận hiệu chỉnh
đánh dấu báo cáo cũ chưa xác minh. Đây là kiểm chứng luồng cơ bản, không đảm bảo
độ chính xác của mọi nhiệm vụ nghiên cứu. Flash đầy đủ thử với ngữ cảnh bot bị
timeout; bản này giữ Flash-Lite. Quota free tier vẫn có thể chặn các lượt dồn dập.

Beta.17 mô tả bridge theo ứng dụng tổng quát, áp dụng cho bot nghiên cứu và coding
client; không giả định tool luôn chạy trên máy người dùng. Nếu lần sửa định dạng
hoàn tất vẫn trả văn bản thường, lượt `auto`/`none` nhận nguyên văn dưới dạng text
và kết thúc, không chuyển nội dung đó thành lệnh tool. Phản hồi giống JSON bị hỏng,
sai nonce/schema, lượt bắt buộc gọi tool hoặc stream chưa hoàn tất vẫn báo lỗi.
Điều này giúp hiển thị lời từ chối của Muse thay vì retry lỗi JSON; không chứng
minh Muse chấp nhận điều khiển tool của Rakazo. Live probe hiện vẫn bị từ chối ở
luồng ứng dụng; cần kiểm chứng lại tool calling trước khi coi tích hợp hoạt động.
Live probe tách phiên tạo hai session mới, xác nhận ID khác nhau và lịch sử rỗng.
Thử trong nhóm Rakazo hiện có đã hiển thị lời từ chối thay vì lỗi JSON và kết thúc
lượt; đây chưa phải kết quả nghiên cứu thành công. Yêu cầu dry-run chọn một tool
với dữ liệu thử ngắn cũng bị Muse từ chối.

Beta.14 bổ sung bước đánh thức VM qua `/api/hatch/vm/wake` trước khi lấy token và
mở socket, theo luồng Muse Web. Dùng cùng tài khoản/proxy, timeout 10 giây;
không chứa nội dung chat. Wake bị từ chối thì dừng trước khi tạo token hoặc gửi
chat. VM bị DISABLED hoặc ID không hợp lệ không được đánh thức. Không cần giữ
Muse Web mở để thực hiện bước wake; giới hạn retry trước khi gửi chat vẫn giữ nguyên.

Beta.13 giữ mã HTTP khi gateway Muse từ chối mở WebSocket và chỉ thử lại tối đa
3 lần với HTTP 502/503/504 trước khi gửi Noise hoặc nội dung chat, trong cùng
giới hạn 20 giây. Không thử lại lỗi đăng nhập/429 hay gửi lại tin nhắn đã nộp.
Log tạo phiên cũng ghi lỗi trước khi gửi chat; cookie tùy chọn 0/1 không còn làm
hỏng mã HTTP và thời gian trong chẩn đoán.

Beta.12 chỉ còn Muse/Gemini. Khi nâng cấp, app xóa nguồn đã gỡ và thông tin
đăng nhập của nguồn đó khỏi cấu hình DPAPI; các tài khoản còn lại được giữ nguyên.
Nếu nguồn đã gỡ đang được chọn, kết nối tự động tắt; hãy chọn Muse/Gemini để dùng tiếp.

Beta.10 có **Model / tư duy** ở tab Tài khoản: chọn số tài khoản Google trong URL
(`/u/5/app` → 5), tải danh sách model/quyền truy cập từ acc đó, rồi chọn mức tư
duy. Model khóa không thể lưu. Số tài khoản được dùng cho model, quota và chat;
cookie Google có thể chứa nhiều phiên đăng nhập, nên cookie đúng chưa đủ để xác
định số acc. Đổi lựa chọn của acc đang chạy sẽ dừng kết nối và đổi API key, cần
kết nối lại rồi mở Claude mới. Muse không đổi.

Beta.9 bật nút **Kiểm tra quota** cho Gemini Web: % đã dùng trong cửa sổ 5 giờ và
hàng tuần, kèm giờ reset khi Google cung cấp. Chỉ gọi khi bấm, dùng đúng cookie/
proxy từng acc, không đổi acc đang chạy hay tạo chat. Chi tiết, nguồn giao thức
và giới hạn kiểm chứng nằm ở [GEMINI_WEB.md](GEMINI_WEB.md#kiểm-tra-quota).

Beta.8 thêm lựa chọn **Gemini Web** khi thêm tài khoản; mặc định tài khoản cũ vẫn
là Muse. Chỉ một backend chạy tại một thời điểm, không chia sẻ cookie/session
hoặc tự chuyển provider khi lỗi. Chi tiết và giới hạn: [GEMINI_WEB.md](GEMINI_WEB.md).
Beta.3 bổ sung socket chuẩn bị sẵn, probe theo idle và tối ưu chờ hoàn tất có điều
kiện. Số đo và giới hạn được ghi trong [PERFORMANCE.md](PERFORMANCE.md).
Beta.4 gửi toàn bộ ảnh của request khôi phục theo một batch và tự chia đôi phần
bị từ chối/thất bại rõ ràng. Timeout hoặc mất kết nối không kích hoạt gửi lại.

Beta.7 bổ sung proxy HTTP/HTTPS riêng cho mỗi acc trong **Tài khoản Muse → Sửa**.
Nhập `http://host:port`, `http://user:password@host:port` hoặc `https://host:port`;
để trống để đi trực tiếp. Username/password chứa ký tự đặc biệt cần URL-encode.
Cột Proxy chỉ hiển thị host/cổng, không hiện thông tin đăng nhập. Proxy được mã
hóa DPAPI cùng cookie. Chưa hỗ trợ SOCKS5.

Xác thực Muse, WebSocket/Noise và kiểm tra quota dùng đúng proxy của acc đã chọn.
Proxy lỗi thì báo lỗi, không tự kết nối trực tiếp; chế độ trực tiếp không kế thừa
proxy hệ thống. Thay proxy của acc đang chạy dừng kết nối cũ, cần bấm Kết nối lại.
Chuyển acc thay cả cookie/node/proxy và tạo lại session/socket. Proxy này không
đổi cổng localhost giữa Claude và Muse Desk, không định tuyến tool Exa/Claude.

Đã kiểm tra proxy CONNECT cục bộ có xác thực, Noise round-trip qua tunnel,
407/502/header quá lớn, HTTPS proxy từ chối TLS không hợp lệ; quota từng acc và
biến môi trường của Rust child thật. Chưa thử proxy thương mại hoặc xác minh IP
ra ngoài vì chưa có proxy của người dùng. Proxy không bảo đảm tránh giới hạn hay
khóa tài khoản Muse.

Bản xem trước có màn Bắt đầu, Tài khoản Muse, Cài đặt và Hướng dẫn riêng;
tab Tài khoản Muse cho phép lưu sẵn nhiều acc bằng các nút Thêm, Sửa, Xóa và
Chuyển tài khoản. Mỗi lúc chỉ một acc chạy. Cookie từng acc nằm trong cấu hình
DPAPI; cấu hình cũ một acc được chuyển thành mục “Tài khoản hiện tại”. Tên acc
là tên gợi nhớ tự đặt, không phải email do Muse xác minh.

Beta.5 có nút **Kiểm tra quota** ở từng dòng acc. Chỉ khi bấm mới gọi action
`fetchSubscriptionAction` của Muse web (không tạo VM/chat, không đổi tài khoản
đang dùng). App hiển thị gói, % đã dùng, thời điểm reset theo giờ máy và số dư
mua thêm theo nhãn Muse trả về; không quy đổi các bộ đếm nội bộ thành token.
Kết quả giữ riêng từng acc trong RAM cùng giờ kiểm tra. Không tự tải khi mở app,
chọn acc, đổi tab hay theo timer; muốn cập nhật thì bấm lại. Cookie sửa/xóa làm
mất kết quả cũ của acc. Request có timeout 20 giây, giới hạn phản hồi 256 KiB,
không tự retry. Đây là action riêng của web, ID/schema có thể đổi; khi đó app
báo chưa lấy được dữ liệu thay vì hiển thị 0% hoặc quota không giới hạn.

Chuyển acc dừng proxy của app, bật bằng cookie/node riêng, kiểm tra kết nối rồi
mới lưu lựa chọn. Nếu thất bại, app thử phục hồi acc trước. Cache RAM bị xóa
và mỗi acc có file `conversations-<id>.json` riêng. API key đổi khi chuyển acc:
cửa sổ Claude cũ sẽ bị từ chối key để tránh tự gửi lịch sử của acc trước sang
acc mới. Mở phiên Claude mới bằng nút trong app. Các file session cũ vẫn được
giữ lại khi xóa acc khỏi danh sách, việc xóa acc chỉ xóa cookie/cấu hình acc.

Bản xem trước đã kiểm tra tự động migration, mã hóa nhiều acc, phân tách file
session/API key, chuyển thành công và rollback khi lỗi. Giao diện đã kiểm tra
thêm/sửa/xóa và lưu danh sách sau restart với acc thử; chưa xác minh chuyển
qua lại giữa hai tài khoản Muse thật khác nhau.

Các trạng thái kết nối và lý do chưa mở được Claude hiện trên màn hình;
trạng thái kết nối và lý do chưa mở được Claude hiện trên màn hình. Đã kiểm
tra thực tế ở 96 DPI, cửa sổ mặc định,
bao gồm thu/phóng cửa sổ, lỗi thiếu cookie hiện tại chỗ và lưu cài đặt khi
chưa có cookie. Giá trị đã lưu được giữ sau khi mở lại app. Chưa kiểm tra UI
ở mức DPI khác. Chạy trực tiếp bản EXE xem trước để đánh giá giao diện.

Bản `0.1.0-beta.1` là ứng dụng portable cho Windows 10/11 x64. Gói ZIP chứa
`MuseDesk.exe`, hướng dẫn tiếng Việt và giấy phép thư viện. EXE đã kèm server
Rust và .NET runtime; người dùng không cần cài công cụ lập trình hay chạy script.

## Sử dụng

1. Giải nén toàn bộ `dist/MuseDesk-0.1.0-beta.1-win-x64.zip` vào thư mục muốn giữ lại.
2. Mở `MuseDesk.exe`, đăng nhập tài khoản của mình tại <https://muse.ai>.
3. Bấm **Cách lấy cookie**, làm theo hướng dẫn, dán cookie vào app.
4. Bấm **Lưu & kết nối**, đợi trạng thái **Đã kết nối Muse**.
5. Chọn thư mục dự án và bấm **Mở Claude Code**.

Claude Code cần cài riêng. Nút **Cài Claude Code** mở
[hướng dẫn chính thức](https://code.claude.com/docs/en/setup). App truyền cấu
hình proxy cho cửa sổ Claude vừa mở, không sửa PowerShell profile hay cấu hình
Claude toàn máy, và giữ cơ chế hỏi quyền của Claude Code.

Giữ app chạy trong lúc dùng Claude. Đóng cửa sổ khi proxy đang chạy sẽ thu app
xuống khay hệ thống; nhấp đúp biểu tượng để mở lại. **Thoát** dừng server do app
quản lý. Khi khởi động, app kiểm tra cổng và từ chối nếu đã có chương trình khác
sử dụng, không dừng chương trình đó. Cổng mặc định `127.0.0.1:3001`.

Trong **Cài đặt nâng cao** có thể đổi cổng, context budget, tự kết nối khi mở
app, tạo shortcut Desktop và xuất chẩn đoán đã che cookie/key. Không tự chạy
cùng Windows. Cookie hết hạn thì cập nhật cookie rồi kết nối lại.

## Dữ liệu trên máy

`%LOCALAPPDATA%\MuseDesk` chứa:

- `settings.protected`: cookie, khóa proxy và tùy chọn, mã hóa bằng Windows
  DPAPI cho tài khoản Windows hiện tại.
- `runtime/0.1.0-beta.1/`: server và launcher được giải nén từ EXE, kiểm tra
  SHA-256 trước khi sử dụng lại.
- `conversations.json`: ánh xạ session, không chứa nội dung chat hay cookie.

Không chia sẻ file cấu hình. Khi gửi bản ZIP cho người khác, gửi gói trong
`dist`, mỗi người dùng tài khoản Muse riêng. Để gỡ, thoát app rồi xóa thư mục
đã giải nén và shortcut. Xóa thêm `%LOCALAPPDATA%\MuseDesk` nếu muốn xóa cấu hình.

## Phạm vi bản beta

- Chưa có đăng nhập một nút: người dùng cần lấy cookie thủ công.
- Chưa ký số, chưa có bộ cài hay tự cập nhật. Windows có thể cảnh báo ứng dụng
  chưa nhận diện khi tải về.
- Tool calling dùng prompt JSON; SSE của adapter Anthropic đệm tới khi kiểm
  tra JSON xong. Tốc độ phụ thuộc Muse, chưa tương đương API model native.
- Context mặc định 256k là budget cho Claude Code; không xác nhận giới hạn
  context thực tế của tài khoản Muse. Có thể chọn budget khác trong app.
- PNG/JPEG/GIF/WebP tối đa 2 MiB mỗi ảnh; tổng JSON request tối đa 4 MiB.
  Chưa hỗ trợ PDF, document và audio.
- Hai route Anthropic yêu cầu API key, các route OpenAI/conversation cũ chưa
  có auth. Server chỉ bind localhost; không mở cổng ra mạng ngoài.
- Muse Desk là ứng dụng thử nghiệm độc lập, không phải sản phẩm chính thức
  của Muse hay Claude Code.

## Build cho người phát triển

Cần Rust stable với Windows MSVC build tools, .NET SDK 8 trở lên và mạng để
tải dependency. Từ thư mục dự án chạy:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\packaging\build-windows.ps1
```

Script build server Rust `--release --locked` với CRT static cho Windows MSVC,
nhúng server và launcher vào
EXE self-contained, thu thập giấy phép từ Cargo và đúng .NET runtime pack,
chạy kiểm tra offline rồi tạo ZIP và checksum SHA-256 trong `dist`.
Không đọc hoặc đóng gói cấu hình tài khoản, `.local`, source hay build cache.

Source giao diện nằm ở `desktop/MuseDesk`. Backend Rust và launcher terminal
hiện có vẫn sử dụng độc lập được. CLI chat thử nghiệm đã được loại bỏ.

## Kiểm tra bản 0.1.0-beta.1

Đã build và kiểm tra trên máy Windows phát triển, chưa có xác minh trên máy
Windows sạch độc lập. Các kiểm tra tự động gồm lưu/đọc DPAPI, giải nén và bật
server nhúng, che bí mật trong chẩn đoán, từ chối request Anthropic thiếu key,
xung đột cổng không ảnh hưởng listener có sẵn và dừng server trả lại cổng.
Kiểm tra tùy chọn đã xác thực Muse/VM thật và chạy launcher với Claude Code
đã cài bằng `--version`; không phát sinh tin nhắn chat trong bước này.
ZIP cuối đã qua kiểm tra CRC, checksum và quét cookie/key của tài khoản thử;
bản giải nén chạy được kiểm tra offline với PATH chỉ có công cụ Windows.
Đã kiểm tra import DLL của server, không còn phụ thuộc Visual C++ Runtime
cài riêng. Gói ZIP cuối khoảng 68,2 MiB.

Đã kiểm tra giao diện: màn hình mới, cài đặt nâng cao, báo thiếu cookie,
kết nối Muse thật, dừng/kết nối lại và thu xuống khay hệ thống. Khi kết thúc
process giao diện, Windows Job Object dừng server con; server phát triển
đang chạy ở cổng 3000 vẫn được giữ nguyên.

Kết quả an toàn để xem nằm ở `target/MuseDesk-0.1.0-beta.1-smoke.json` và
`target/MuseDesk-live-smoke.json`; kiểm tra ZIP nằm ở
`target/portable-release-check.json`. Các báo cáo không nằm trong gói gửi
người dùng.
