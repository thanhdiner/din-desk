# Độ trễ Muse Desk beta.3

Mặc định dùng profile `balanced`. `MUSE_PERF_PROFILE=baseline` tắt các tối ưu
dưới đây để đối chiếu bằng cùng executable, tài khoản và workload.

- Dùng lại HTTP client xác thực trong một tài khoản, không cache/chia sẻ token
  giữa tài khoản và không giả định token gateway dùng lại được cho nhiều socket.
- Chuẩn bị tối đa hai socket Noise chưa dùng cho chat. Chuyển quyền sở hữu mỗi
  socket cho đúng một conversation; không tái sử dụng context của conversation
  khác. Socket chờ quá 60 giây hoặc khác VM bị bỏ. Không tạo conversation thừa
  để làm nóng pool. Việc nạp pool có giới hạn, không chạy vòng lặp nền vô hạn.
- Nếu socket đang được chuẩn bị, foreground chờ tối đa 3 giây để nhận socket đó
  thay vì tranh bootstrap thêm một kết nối. Lỗi nền vẫn có đường kết nối trực tiếp.
- Socket vừa được xác nhận hoạt động trong 20 giây được dùng ngay. Sau khoảng
  idle đó vẫn probe; nếu probe hỏng thì kết nối lại trước khi gửi chat. Không
  tự gửi lại chat đã có thể được upstream nhận. Đóng socket ngay sau lần kiểm tra
  vẫn là một race mạng có thể xảy ra; đây không phải bảo đảm không mất kết nối.
- Chờ 250 ms chỉ khi root task đã xác nhận completed, active_subagent_count=0,
  không còn task/message đang mở và đã có message hoàn chỉnh. Trường hợp thiếu
  xác nhận giữ 1 giây; fallback history vẫn giữ 3 giây.
- Chuẩn hóa khoảng trắng và dòng trùng liền nhau ngoài code fence trong mô tả
  tool. Giữ nguyên schema, quy tắc và ví dụ code; không cắt theo độ dài. Mức giảm
  byte nhỏ với tool vốn đã gọn. Không cắt system, lịch sử hay tool_result.

## Kiểm chứng

82 regression test qua, ba test phụ thuộc Muse/Claude thật được chạy theo nhu cầu.
Benchmark dùng Claude Code thật, hai subagent cùng đọc sáu file/nhánh rồi ghi
marker riêng. Dùng lại cùng bộ file đầu vào, khởi tạo client/session mới cho mỗi
lượt, xác thực tài khoản như app và chờ cùng 3 giây trước khi bắt đầu đo.

Sau lần chỉnh cuối của pool, kết quả:

| Lượt | Baseline | Balanced |
| --- | ---: | ---: |
| 3 | 44,23 s | 49,61 s |
| 4 | 40,06 s | 27,83 s |
| Trung bình | 42,14 s | 38,72 s |

Cả bốn kết quả đều khớp. Balanced dùng socket chuẩn bị sẵn cho ba conversation;
gần như toàn bộ prepare_ms bằng 0, một lần idle probe 192 ms. Baseline phải chuẩn
bị socket mới khoảng 2–4 giây và probe khoảng 0,2 giây/lượt. Balanced lượt 3 có
13 lượt inference, baseline lượt 3 có 8: model tự chia batch khác nhau. Vì vậy số
đo end-to-end là mẫu nhỏ có nhiễu, không chứng minh mọi task nhanh hơn 8%.
Metadata gốc nằm trong `target/performance-report.json` và các log `target/perf-*`.

## Giao thức code riêng, thử nghiệm

`MUSE_EXPERIMENTAL_RAW_BLOCKS=1` cho phép JSON manifest tham chiếu khối UTF-8
được phân cách bằng marker `@@MUSE_BLOB:<nonce>:<id>@@` và marker END tương ứng.
Chỉ áp dụng cho text.text, Write.content và Edit.old_string/new_string. Sau giải
mã vẫn kiểm tra nonce, tên tool, schema, tool_choice và toàn bộ batch; không
phát tool từng phần. Giới hạn 2 MiB/response, 64 khối, mỗi khối chỉ tham chiếu
một lần để tránh khuếch đại bộ nhớ. Marker sai, thiếu, trùng hoặc dư đều bị từ chối.
JSON bình thường và lượt sửa định dạng cũ vẫn được hỗ trợ.

Chế độ này tắt mặc định. Test parser không thay thế việc kiểm tra Muse thật;
không coi giảm escape là bằng chứng end-to-end nhanh hơn.

Probe Muse thật đã nhận một raw block, giải mã đúng nguyên văn 2.841 byte code
có dấu nháy, backslash và Unicode, rồi validate Write thành công (HTTP 200,
16,19 giây). Không thực thi Write trong probe. Chưa có benchmark code dài đủ
lớn để bật chế độ này mặc định. Marker ASCII dùng thay cho dấu angle bracket
sau khi phiên thử đầu không qua framing và phải quay về JSON repair.
