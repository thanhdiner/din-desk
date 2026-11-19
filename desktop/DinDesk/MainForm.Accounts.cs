namespace MuseDesk;

internal sealed partial class MainForm
{
    private readonly DataGridView accounts = new() {
        Dock = DockStyle.Fill, Name = "Accounts", AccessibleName = "Danh sách tài khoản",
        AllowUserToAddRows = false, AllowUserToDeleteRows = false, AllowUserToResizeRows = false,
        AutoGenerateColumns = false, ReadOnly = true, MultiSelect = false, RowHeadersVisible = false,
        SelectionMode = DataGridViewSelectionMode.FullRowSelect, BackgroundColor = SystemColors.Window,
        BorderStyle = BorderStyle.Fixed3D, AutoSizeRowsMode = DataGridViewAutoSizeRowsMode.None,
        RowTemplate = { Height = 30 }
    };
    private readonly Label activeAccountLabel = UiTheme.Label("Chưa chọn tài khoản");
    private readonly List<Control> accountControls = new();
    private MuseAccount? SelectedAccount => accounts.CurrentRow?.Tag as MuseAccount;
    private readonly AccountQuotaClient quotaClient;
    private readonly CancellationTokenSource quotaLifetime = new();
    private readonly Dictionary<string, AccountQuota> quotas = new();
    private readonly Dictionary<string, string> quotaErrors = new();
    private readonly HashSet<string> quotaPending = new();
    private readonly TextBox quotaDetail = new() { Dock = DockStyle.Fill, ReadOnly = true, Multiline = true,
        ScrollBars = ScrollBars.Vertical, Name = "QuotaDetail", AccessibleName = "Quota tài khoản đã chọn" };

    private Control SavedAccountsPage()
    {
        var page = Stack();
        Add(page, activeAccountLabel,28,4);
        Add(page, UiTheme.Label("Bấm Kiểm tra quota ở acc cần xem. Chỉ lấy dữ liệu khi bấm, không tự chuyển acc."),26,4);
        accounts.Columns.Add(new DataGridViewTextBoxColumn { Name = "AccountName", HeaderText = "Tài khoản", AutoSizeMode = DataGridViewAutoSizeColumnMode.Fill, MinimumWidth = 100, SortMode = DataGridViewColumnSortMode.NotSortable });
        accounts.Columns.Add(new DataGridViewTextBoxColumn { Name = "AccountProvider", HeaderText = "Nguồn", Width = 72, SortMode = DataGridViewColumnSortMode.NotSortable });
        accounts.Columns.Add(new DataGridViewTextBoxColumn { Name = "AccountProxy", HeaderText = "Proxy", Width = 140, SortMode = DataGridViewColumnSortMode.NotSortable });
        accounts.Columns.Add(new DataGridViewTextBoxColumn { Name = "Quota", HeaderText = "Quota gần nhất", Width = 170, SortMode = DataGridViewColumnSortMode.NotSortable });
        accounts.Columns.Add(new DataGridViewButtonColumn { Name = "CheckQuota", HeaderText = "Thao tác", Width = 125, FlatStyle = FlatStyle.System, SortMode = DataGridViewColumnSortMode.NotSortable });
        accounts.CellContentClick += async (_, e) => {
            if (e.RowIndex >= 0 && e.ColumnIndex == accounts.Columns["CheckQuota"].Index && accounts.Rows[e.RowIndex].Tag is MuseAccount account)
                await CheckQuotaAsync(account);
        };
        Add(page,accounts,126,6);
        Add(page,quotaDetail,82,6);
        var editRow=Flow();
        foreach (var (text,action) in new (string,Action)[] {
            ("Thêm",()=>EditAccount(false)),("Sửa",()=>EditAccount(true)),("Xóa",DeleteAccount) })
        {
            var button=Action(text,width:90); button.Click+=(_,_)=> BeginInvoke(new Action(() => { if (!busy) { try {action();} catch(Exception error) {Notify(error is ArgumentException?error.Message:"Không lưu được tài khoản. Kiểm tra quyền ghi thư mục.",true);} } }));
            editRow.Controls.Add(button); accountControls.Add(button);
        }
        var guide=Action("Cách lấy cookie",width:140); guide.Click+=(_,_)=>Navigate(3); editRow.Controls.Add(guide);
        var models=Action("Model / tư duy",width:135);models.Click+=async(_,_)=>await EditGeminiModelAsync();editRow.Controls.Add(models);accountControls.Add(models);
        Add(page,editRow,38,8);
        var row=Flow();
        connect.Text="Chuyển tài khoản";
        connect.Click+=async(_,_)=>await RunAction(ConnectAsync);
        check.Click+=async(_,_)=>await RunAction(async()=> { await server.CheckMuseAsync(); verified=true;Notify("Tài khoản đang dùng kết nối tốt."); });
        stop.Click+=(_,_)=>StopServer(); row.Controls.AddRange(new Control[]{connect,check,stop});
        Add(page,row,38,8);
        Add(page,UiTheme.Label("Chỉ một acc hoạt động. Chuyển acc thì mở phiên Claude mới. Cookie được mã hóa trên Windows."),30);
        accounts.SelectionChanged+=(_,_)=> { UpdateQuotaDetail(); if(!busy) UpdateStatus(); };
        // SelectionChanged runs before CurrentRow moves to the new cell.
        accounts.CurrentCellChanged+=(_,_)=> { UpdateQuotaDetail(); if(!busy) UpdateStatus(); };
        RefreshAccounts(); accountControls.Add(accounts);
        // Account rows/details remain accessible at the minimum window size.
        page.MinimumSize = new Size(0, 410);
        var scroll = new Panel { Dock = DockStyle.Fill, AutoScroll = true };
        scroll.Controls.Add(page);
        return scroll;
    }

    private void RefreshAccounts(string? selected = null)
    {
        selected ??= SelectedAccount?.Id ?? settings.ActiveAccountId;
        accounts.Rows.Clear();
        foreach(var account in settings.Accounts) {
            int index = accounts.Rows.Add(account.Name, account.ProviderLabel, AccountProxy.Display(account.ProxyUrl), QuotaSummary(account.Id), quotaPending.Contains(account.Id) ? "Đang lấy…" : "Kiểm tra quota");
            accounts.Rows[index].Tag = account;
        }
        int chosen = settings.Accounts.FindIndex(a=>a.Id==selected);
        if (chosen < 0 && accounts.Rows.Count > 0) chosen = 0;
        if (chosen >= 0) accounts.CurrentCell = accounts.Rows[chosen].Cells[0];
        UpdateQuotaDetail();
    }

    private string QuotaSummary(string id) => quotaPending.Contains(id) ? "Đang lấy…"
        : quotaErrors.ContainsKey(id) ? "Kiểm tra thất bại"
        : quotas.TryGetValue(id, out var quota) ? quota.Summary : "Chưa kiểm tra";

    private void UpdateQuotaDetail()
    {
        if (SelectedAccount is not { } account) { quotaDetail.Text = "Chọn một tài khoản để xem quota."; return; }
        string detail = quotas.TryGetValue(account.Id, out var quota) ? quota.Detail : "Chưa kiểm tra. Bấm Kiểm tra quota ở dòng tài khoản này.";
        if (quotaErrors.TryGetValue(account.Id, out string? error)) detail = error + Environment.NewLine + (quota == null ? "" : "Kết quả cũ: " + detail);
        if (quotaPending.Contains(account.Id)) detail = $"Đang lấy quota từ {account.ProviderLabel}…" + Environment.NewLine + detail;
        string selection=account.Provider=="gemini"?$" · Google {account.GeminiAccountIndex} · {(account.GeminiModelName.Length>0?account.GeminiModelName:"Model mặc định")}":"";
        if(account.Provider=="gemini" && account.GeminiThinkingName.Length>0)selection+=" · "+account.GeminiThinkingName;
        quotaDetail.Text = account.Name + selection + Environment.NewLine + detail;
    }

    private async Task CheckQuotaAsync(MuseAccount account)
    {
        if (busy || !quotaPending.Add(account.Id)) return;
        string id = account.Id, cookie = account.Cookie, proxyUrl = account.ProxyUrl, provider = account.Provider;
        int googleIndex=account.GeminiAccountIndex;
        bool SameCredential() => settings.Accounts.Any(a => a.Id == id && a.Cookie == cookie && a.ProxyUrl == proxyUrl && a.Provider == provider && a.GeminiAccountIndex==googleIndex);
        void RefreshQuotaRow() {
            foreach (DataGridViewRow row in accounts.Rows)
                if ((row.Tag as MuseAccount)?.Id == id) {
                    row.Cells["Quota"].Value = QuotaSummary(id);
                    row.Cells["CheckQuota"].Value = quotaPending.Contains(id) ? "Đang lấy…" : "Kiểm tra quota";
                }
            UpdateQuotaDetail();
        }
        quotaErrors.Remove(id); RefreshQuotaRow();
        try {
            var result = await quotaClient.FetchAccountAsync(account, quotaLifetime.Token);
            if (SameCredential()) quotas[id] = result;
        }
        catch (OperationCanceledException) {
            if (!quotaLifetime.IsCancellationRequested && SameCredential()) quotaErrors[id] = "Lấy quota quá lâu. Kiểm tra mạng rồi bấm lại.";
        }
        catch (Exception error) {
            if (SameCredential()) quotaErrors[id] = error is InvalidOperationException or ArgumentException ? error.Message : "Không lấy được quota. Kiểm tra mạng rồi bấm lại.";
        }
        finally {
            quotaPending.Remove(id);
            if (!IsDisposed && !Disposing) RefreshQuotaRow();
        }
    }

    private void EditAccount(bool edit)
    {
        var old=edit?SelectedAccount:null;
        if(edit && old==null) {Notify("Chọn tài khoản cần sửa.");return;}
        using var dialog = new AccountDialog(old);
        if(dialog.ShowDialog(this)!=DialogResult.OK) return;
        var candidate=settings.Copy();
        var edited=dialog.Account!;
        if(old!=null) candidate.Accounts[candidate.Accounts.FindIndex(a=>a.Id==old.Id)]=edited;
        else candidate.Accounts.Add(edited);
        // Editing an active credential stops its owned process so the UI cannot
        // claim the old connection is using the newly saved cookie.
        bool credentialChanged=old!=null && old.Id==settings.ActiveAccountId && (old.Cookie!=edited.Cookie || old.ProxyUrl!=edited.ProxyUrl);
        if(credentialChanged)candidate.ApiKey=Convert.ToHexString(System.Security.Cryptography.RandomNumberGenerator.GetBytes(32));
        store.Save(candidate);
        if(credentialChanged) {server.Stop();verified=false;}
        if(old!=null && (old.Cookie!=edited.Cookie || old.ProxyUrl!=edited.ProxyUrl)) { quotas.Remove(old.Id); quotaErrors.Remove(old.Id); }
        settings=candidate; RefreshAccounts(edited.Id); UpdateStatus();
        Notify(credentialChanged?"Đã cập nhật kết nối. Bấm Kết nối lại để áp dụng.":"Đã lưu tài khoản. Chọn acc rồi bấm Chuyển tài khoản để sử dụng.");
    }

    private Task EditGeminiModelAsync()
    {
        if(busy)return Task.CompletedTask;
        if(SelectedAccount is not {Provider:"gemini"} account){Notify("Chọn tài khoản Gemini để đổi model/tư duy.");return Task.CompletedTask;}
        using var dialog=new GeminiModelDialog(account,store.DirectoryPath);
        if(dialog.ShowDialog(this)!=DialogResult.OK)return Task.CompletedTask;
        try {
            var candidate=settings.Copy();var edited=candidate.Accounts.Single(a=>a.Id==account.Id);
            bool changed=edited.GeminiAccountIndex!=dialog.AccountIndex || edited.GeminiModelId!=dialog.ModelId || edited.GeminiThinkingId!=dialog.ThinkingId;
            edited.GeminiAccountIndex=dialog.AccountIndex;edited.GeminiModelId=dialog.ModelId;edited.GeminiThinkingId=dialog.ThinkingId;
            edited.GeminiModelName=dialog.ModelName;edited.GeminiThinkingName=dialog.ThinkingName;
            if(changed && account.Id==settings.ActiveAccountId)candidate.ApiKey=Convert.ToHexString(System.Security.Cryptography.RandomNumberGenerator.GetBytes(32));
            store.Save(candidate);
            if(changed && account.Id==settings.ActiveAccountId){server.Stop();verified=false;}
            if(account.GeminiAccountIndex!=dialog.AccountIndex){quotas.Remove(account.Id);quotaErrors.Remove(account.Id);}
            settings=candidate;RefreshAccounts(account.Id);UpdateStatus();
            Notify(changed?"Đã lưu model/tư duy. Kết nối lại và mở phiên Claude mới để áp dụng.":"Đã lưu lựa chọn Gemini.");
        }catch(Exception error){Notify(error is ArgumentException?error.Message:"Không lưu được lựa chọn Gemini.",true);}
        return Task.CompletedTask;
    }

    private void DeleteAccount()
    {
        if(SelectedAccount is not MuseAccount account) {Notify("Chọn tài khoản cần xóa.");return;}
        bool active=account.Id==settings.ActiveAccountId;
        if(MessageBox.Show(this,active?"Xóa tài khoản đang dùng sẽ dừng kết nối. Tiếp tục?":"Xóa tài khoản đã chọn khỏi danh sách?", "Xóa tài khoản",MessageBoxButtons.YesNo,MessageBoxIcon.Question)!=DialogResult.Yes)return;
        var candidate=settings.Copy();candidate.Accounts.RemoveAll(a=>a.Id==account.Id);
        if(active) {candidate.ActiveAccountId=null;candidate.Cookie="";}
        store.Save(candidate);
        if(active) {server.Stop();verified=false;}
        quotas.Remove(account.Id); quotaErrors.Remove(account.Id);
        settings=candidate;RefreshAccounts();UpdateStatus();Notify("Đã xóa tài khoản khỏi danh sách.");
    }
}

internal sealed class AccountDialog : Form
{
    public MuseAccount? Account { get; private set; }
    public AccountDialog(MuseAccount? previous)
    {
        Text=previous==null?"Thêm tài khoản":"Sửa tài khoản";
        StartPosition=FormStartPosition.CenterParent;FormBorderStyle=FormBorderStyle.FixedDialog;
        MaximizeBox=MinimizeBox=false;ShowInTaskbar=false;ClientSize=new Size(560,404);
        Font=SystemFonts.MessageBoxFont;BackColor=SystemColors.Window;
        var name=new TextBox {Text=previous?.Name??"",Name="AccountName",AccessibleName="Tên tài khoản",Dock=DockStyle.Fill,MaxLength=80};
        var provider=new ComboBox {Name="AccountProvider",AccessibleName="Nguồn tài khoản",Dock=DockStyle.Fill,DropDownStyle=ComboBoxStyle.DropDownList,Enabled=previous==null};
        provider.Items.AddRange(new object[]{"Muse","Gemini Web"});provider.SelectedIndex=previous?.Provider=="gemini"?1:0;
        var cookie=new TextBox {Text=previous?.Cookie??"",Name="AccountCookie",AccessibleName="Cookie tài khoản",UseSystemPasswordChar=true,Dock=DockStyle.Fill};
        var proxy=new TextBox {Text=previous?.ProxyUrl??"",Name="AccountProxyUrl",AccessibleName="Proxy tài khoản",UseSystemPasswordChar=true,Dock=DockStyle.Fill,MaxLength=4096};
        var layout=new TableLayoutPanel {Dock=DockStyle.Fill,Padding=new Padding(12),ColumnCount=1,RowCount=12};
        foreach(int height in new[]{22,30,22,30,22,30,30,22,30,24,34,64})layout.RowStyles.Add(new RowStyle(SizeType.Absolute,height));
        layout.ColumnStyles.Add(new ColumnStyle(SizeType.Percent,100));
        layout.Controls.Add(new Label {Text="Tên gợi nhớ (ví dụ: Cá nhân, Dự phòng)",AutoSize=true},0,0);layout.Controls.Add(name,0,1);
        layout.Controls.Add(new Label {Text="Nguồn (tạo tài khoản mới để đổi nguồn)",AutoSize=true},0,2);layout.Controls.Add(provider,0,3);
        var cookieLabel=new Label {AutoSize=true};
        void UpdateCookieLabel() {
            cookieLabel.Text=provider.SelectedIndex==1?"Cookie Gemini (__Secure-1PSID, __Secure-1PSIDTS)":"Cookie Muse (có hatch_sess)";
        }
        provider.SelectedIndexChanged+=(_,_)=>UpdateCookieLabel();UpdateCookieLabel();
        layout.Controls.Add(cookieLabel,0,4);layout.Controls.Add(cookie,0,5);
        var show=new CheckBox {Text="Hiện cookie",AutoSize=true};show.CheckedChanged+=(_,_)=>cookie.UseSystemPasswordChar=!show.Checked;layout.Controls.Add(show,0,6);
        layout.Controls.Add(new Label {Text="Proxy HTTP/HTTPS (để trống = trực tiếp)",AutoSize=true},0,7);layout.Controls.Add(proxy,0,8);
        var showProxy=new CheckBox {Text="Hiện proxy",AutoSize=true};showProxy.CheckedChanged+=(_,_)=>proxy.UseSystemPasswordChar=!showProxy.Checked;layout.Controls.Add(showProxy,0,9);
        var buttons=new FlowLayoutPanel {Dock=DockStyle.Fill,FlowDirection=FlowDirection.RightToLeft};
        var cancel=new Button {Text="Hủy",DialogResult=DialogResult.Cancel};var save=new Button {Text="Lưu"};
        var error=new Label {Dock=DockStyle.Fill,UseMnemonic=false};
        save.Click+=(_,_)=> {
            try {
                string value=cookie.Text.Trim(); if(value.StartsWith("Cookie:",StringComparison.OrdinalIgnoreCase))value=value[7..].Trim();
                string source=provider.SelectedIndex==1?"gemini":"muse";
                SettingsStore.ValidateProviderCookie(value,source);
                string proxyValue=proxy.Text.Trim(); AccountProxy.Parse(proxyValue);
                if(string.IsNullOrWhiteSpace(name.Text))throw new ArgumentException("Nhập tên gợi nhớ cho tài khoản.");
                Account=new MuseAccount {Id=previous?.Id??Guid.NewGuid().ToString("N"),NodeId=previous?.NodeId??("musedesk-"+Guid.NewGuid().ToString("N")),Name=name.Text.Trim(),Cookie=value,ProxyUrl=proxyValue,Provider=source,
                    GeminiAccountIndex=previous?.GeminiAccountIndex??0,GeminiModelId=previous?.GeminiModelId??"",GeminiThinkingId=previous?.GeminiThinkingId??"",
                    GeminiModelName=previous?.GeminiModelName??"",GeminiThinkingName=previous?.GeminiThinkingName??""};
                DialogResult=DialogResult.OK;Close();
            }catch(ArgumentException e){error.Text=e.Message;}
        };
        buttons.Controls.AddRange(new Control[]{cancel,save});layout.Controls.Add(buttons,0,10);layout.Controls.Add(error,0,11);
        Controls.Add(layout);AcceptButton=save;CancelButton=cancel;
    }
}
