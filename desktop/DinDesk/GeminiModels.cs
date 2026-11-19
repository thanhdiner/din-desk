using System.Diagnostics;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace MuseDesk;

internal sealed record GeminiThinkingChoice(
    [property: JsonPropertyName("id")] string Id,
    [property: JsonPropertyName("name")] string Name)
{
    public override string ToString()=>Name;
}
internal sealed record GeminiModelChoice(
    [property: JsonPropertyName("id")] string Id,
    [property: JsonPropertyName("name")] string Name,
    [property: JsonPropertyName("description")] string Description,
    [property: JsonPropertyName("available")] bool Available,
    [property: JsonPropertyName("levels")] GeminiThinkingChoice[] Levels)
{
    public override string ToString()=>Name+(Available?"":" (bị khóa)");
}
internal sealed record GeminiModelCatalog(
    [property: JsonPropertyName("account_index")] int AccountIndex,
    [property: JsonPropertyName("models")] GeminiModelChoice[] Models);

internal static class GeminiModels
{
    internal static async Task<GeminiModelCatalog> FetchAsync(MuseAccount account,string directory,CancellationToken token)
    {
        SettingsStore.ValidateProviderCookie(account.Cookie,"gemini");
        SettingsStore.ValidateGeminiSelection(account.GeminiAccountIndex,"","");
        string runtime=RuntimeFiles.Extract(directory);
        var start=new ProcessStartInfo(Path.Combine(runtime,"muse-api.exe")) {
            UseShellExecute=false,CreateNoWindow=true,WorkingDirectory=directory,
            RedirectStandardOutput=true,RedirectStandardError=true
        };
        start.ArgumentList.Add("--gemini-models");
        start.Environment["GEMINI_COOKIE"]=account.Cookie;
        start.Environment["GEMINI_ACCOUNT_INDEX"]=account.GeminiAccountIndex.ToString(System.Globalization.CultureInfo.InvariantCulture);
        start.Environment["GEMINI_MODEL_ID"]=start.Environment["GEMINI_THINKING_ID"]="";
        start.Environment["MUSE_UPSTREAM_PROXY"]=account.ProxyUrl;
        start.Environment["MUSE_COOKIE"]=start.Environment["MUSE_PROXY_API_KEY"]="";
        using var child=new Process{StartInfo=start};
        using var job=new ProcessJob();
        using var deadline=CancellationTokenSource.CreateLinkedTokenSource(token);deadline.CancelAfter(TimeSpan.FromSeconds(50));
        bool started=false;
        try {
            child.Start();started=true;job.Assign(child);
            // Drain both pipes, bounded; no upstream response/credentials in UI errors.
            var stdout=ReadBoundedAsync(child.StandardOutput,deadline.Token);
            var stderr=ReadBoundedAsync(child.StandardError,deadline.Token);
            await child.WaitForExitAsync(deadline.Token);
            string json=await stdout;_=await stderr;
            if(child.ExitCode!=0)throw new InvalidOperationException("Không tải được model Gemini. Kiểm tra số tài khoản Google, cookie và proxy rồi thử lại.");
            var result=JsonSerializer.Deserialize<GeminiModelCatalog>(json);
            if(result==null || result.AccountIndex!=account.GeminiAccountIndex || result.Models is not {Length:>0 and <=128}
                || result.Models.Any(m=>string.IsNullOrWhiteSpace(m.Id)||string.IsNullOrWhiteSpace(m.Name)||m.Levels==null))
                throw new InvalidOperationException("Danh sách model Gemini không hợp lệ. Cập nhật app nếu Google đổi giao thức.");
            return result;
        }catch(JsonException){throw new InvalidOperationException("Không đọc được danh sách model Gemini. Hãy cập nhật app.");}
        finally {if(started && !child.HasExited)child.Kill(entireProcessTree:true);}
    }
    private static async Task<string> ReadBoundedAsync(StreamReader source,CancellationToken token)
    {
        var text=new System.Text.StringBuilder();var buffer=new char[4096];int count;
        while((count=await source.ReadAsync(buffer.AsMemory(),token))>0){if(text.Length+count>128*1024)throw new InvalidOperationException("Danh sách model Gemini quá lớn.");text.Append(buffer,0,count);}
        return text.ToString();
    }
}

internal sealed class GeminiModelDialog : Form
{
    private readonly NumericUpDown index=new(){Name="GeminiAccountIndex",AccessibleName="Số tài khoản Google",Minimum=0,Maximum=99,Dock=DockStyle.Fill};
    private readonly ComboBox model=new(){Name="GeminiModel",AccessibleName="Model Gemini",DropDownStyle=ComboBoxStyle.DropDownList,Dock=DockStyle.Fill};
    private readonly ComboBox thinking=new(){Name="GeminiThinking",AccessibleName="Mức tư duy",DropDownStyle=ComboBoxStyle.DropDownList,Dock=DockStyle.Fill};
    private readonly Label status=new(){Dock=DockStyle.Fill,UseMnemonic=false};
    private readonly Button load=new(){Name="ReloadGeminiModels",Text="Tải danh sách",AutoSize=true};
    private readonly Button save=new(){Name="SaveGeminiModel",Text="Lưu",AutoSize=true};
    private readonly CancellationTokenSource lifetime=new();
    private readonly MuseAccount account;
    private readonly string directory;
    private readonly Func<MuseAccount,string,CancellationToken,Task<GeminiModelCatalog>> fetch;
    private bool loading;
    private bool released;
    private int loadedIndex=-1;
    public int AccountIndex=>(int)index.Value;
    public string ModelId=>(model.SelectedItem as GeminiModelChoice)?.Id??"";
    public string ThinkingId=>(thinking.SelectedItem as GeminiThinkingChoice)?.Id??"";
    public string ModelName=>(model.SelectedItem as GeminiModelChoice)?.Name??"";
    public string ThinkingName=>(thinking.SelectedItem as GeminiThinkingChoice)?.Name??"";

    internal GeminiModelDialog(MuseAccount account,string directory,Func<MuseAccount,string,CancellationToken,Task<GeminiModelCatalog>>? fetch=null)
    {
        this.account=account;this.directory=directory;this.fetch=fetch??GeminiModels.FetchAsync;
        Text="Model và tư duy Gemini";Name="GeminiModelDialog";StartPosition=FormStartPosition.CenterParent;
        Font=SystemFonts.MessageBoxFont;AutoScaleMode=AutoScaleMode.Dpi;AutoScaleDimensions=new SizeF(96,96);
        ClientSize=new Size(560,370);FormBorderStyle=FormBorderStyle.FixedDialog;MinimizeBox=MaximizeBox=false;
        index.Value=account.GeminiAccountIndex;
        var layout=new TableLayoutPanel{Dock=DockStyle.Fill,Padding=new Padding(12),ColumnCount=1,RowCount=10};
        foreach(int height in new[]{38,34,24,30,24,30,24,30,72,40})layout.RowStyles.Add(new RowStyle(SizeType.Absolute,height));
        layout.ColumnStyles.Add(new ColumnStyle(SizeType.Percent,100));Controls.Add(layout);
        layout.Controls.Add(new Label{Text="Số tài khoản Google trong URL: /u/5/app thì chọn 5.\r\nModel, quota và chat đều dùng số này.",Dock=DockStyle.Fill},0,0);
        var row=new TableLayoutPanel{Dock=DockStyle.Fill,ColumnCount=2,RowCount=1};row.ColumnStyles.Add(new ColumnStyle(SizeType.Percent,100));row.ColumnStyles.Add(new ColumnStyle(SizeType.Absolute,125));
        row.Controls.Add(index,0,0);row.Controls.Add(load,1,0);layout.Controls.Add(row,0,1);
        layout.Controls.Add(new Label{Text="Model",AutoSize=true},0,2);layout.Controls.Add(model,0,3);
        layout.Controls.Add(new Label{Text="Cấp độ tư duy",AutoSize=true},0,4);layout.Controls.Add(thinking,0,5);
        layout.Controls.Add(new Label{Text="Model bị khóa chỉ để tham khảo, không thể lưu để dùng.",AutoSize=true},0,6);
        layout.Controls.Add(status,0,8);
        var actions=new FlowLayoutPanel{Dock=DockStyle.Fill,FlowDirection=FlowDirection.RightToLeft};var cancel=new Button{Text="Hủy",DialogResult=DialogResult.Cancel,AutoSize=true};
        actions.Controls.Add(cancel);actions.Controls.Add(save);layout.Controls.Add(actions,0,9);
        CancelButton=cancel;AcceptButton=save;
        load.Click+=async(_,_)=>await LoadAsync();
        index.ValueChanged+=(_,_)=>{save.Enabled=false;status.Text="Bấm Tải danh sách để lấy model của đúng số tài khoản này.";};
        model.SelectedIndexChanged+=(_,_)=>SelectModel();
        save.Click+=(_,_)=>{if(!loading && loadedIndex==AccountIndex && model.SelectedItem is GeminiModelChoice {Available:true}){DialogResult=DialogResult.OK;Close();}};
        Shown+=async(_,_)=>await LoadAsync();
        save.Enabled=false;
    }
    private async Task LoadAsync()
    {
        if(loading)return;loading=true;load.Enabled=index.Enabled=model.Enabled=thinking.Enabled=save.Enabled=false;status.Text="Đang tải model của tài khoản Google…";
        try {
            var snapshot=JsonSerializer.Deserialize<MuseAccount>(JsonSerializer.Serialize(account))!;snapshot.GeminiAccountIndex=AccountIndex;
            var catalog=await fetch(snapshot,directory,lifetime.Token);
            if(IsDisposed||lifetime.IsCancellationRequested)return;
            loadedIndex=catalog.AccountIndex;model.Items.Clear();
            model.Items.Add(new GeminiModelChoice("","Mặc định của Google","",true,Array.Empty<GeminiThinkingChoice>()));
            model.Items.AddRange(catalog.Models.Cast<object>().ToArray());
            int selected=Array.FindIndex(model.Items.Cast<GeminiModelChoice>().ToArray(),m=>m.Id==account.GeminiModelId);
            model.SelectedIndex=selected<0?0:selected;
            if(selected<0 && account.GeminiModelId.Length>0)status.Text="Model cũ không còn trong danh sách. Chọn lại trước khi lưu.";
        }catch(OperationCanceledException){if(!IsDisposed)status.Text="Tải danh sách quá lâu. Kiểm tra mạng rồi thử lại.";}
        catch(Exception error){if(!IsDisposed)status.Text=error is InvalidOperationException or ArgumentException?error.Message:"Không tải được danh sách model. Kiểm tra cookie và proxy.";}
        finally {loading=false;if(!IsDisposed){load.Enabled=index.Enabled=true;model.Enabled=model.Items.Count>0;thinking.Enabled=model.Items.Count>0;save.Enabled=loadedIndex==AccountIndex && model.SelectedItem is GeminiModelChoice {Available:true};}}
    }
    private void SelectModel()
    {
        thinking.Items.Clear();thinking.Items.Add(new GeminiThinkingChoice("","Mặc định của model"));
        if(model.SelectedItem is GeminiModelChoice selected){
            thinking.Items.AddRange(selected.Levels.Cast<object>().ToArray());
            int chosen=selected.Id==account.GeminiModelId?Array.FindIndex(thinking.Items.Cast<GeminiThinkingChoice>().ToArray(),t=>t.Id==account.GeminiThinkingId):0;
            thinking.SelectedIndex=chosen<0?0:chosen;
            status.Text=selected.Available?(selected.Description.Length>0?selected.Description+Environment.NewLine:"")+"Lưu xong, kết nối lại và mở phiên Claude mới để áp dụng.":"Model này bị khóa hoặc không được menu của tài khoản cho phép.";
            save.Enabled=!loading && loadedIndex==AccountIndex && selected.Available;
        }
    }
    protected override void Dispose(bool disposing){if(disposing && !released){released=true;lifetime.Cancel();lifetime.Dispose();}base.Dispose(disposing);}
}
