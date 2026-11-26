using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Text.Json;

namespace MuseDesk;

internal static class ProviderTests
{
    public static async Task RunAsync(string root,List<string> checks)
    {
        void Require(bool value,string error){if(!value)throw new InvalidOperationException(error);}
        var settings=new Settings {Cookie="hatch_sess=provider-muse-cookie"};settings.NormalizeAccounts();
        Require(settings.Provider=="muse" && settings.Accounts[0].Provider=="muse","Legacy Muse provider changed");
        var previous=settings.Copy();
        var account=new MuseAccount {Name="Gemini Fixture",Provider="gemini",Cookie="__Secure-1PSID=fixture-gemini-secret; __Secure-1PSIDTS=fixture-ts",ProxyUrl="http://fixture:private-proxy@127.0.0.1:8080"};
        settings.Accounts.Add(account);string key=settings.ApiKey;settings.SelectAccount(account.Id);
        Require(settings.Provider=="gemini" && settings.ModelId=="gemini" && settings.Cookie==account.Cookie && settings.ApiKey==key,"Provider switch mixed model/cookie or changed the proxy key");
        Require(ServerController.ConversationPath(root,settings)!=ServerController.ConversationPath(root,previous),"Provider conversation stores overlap");
        var store=new SettingsStore(root);store.Save(settings);var loaded=store.Load();
        Require(loaded.Provider=="gemini" && loaded.Accounts[0].Provider=="muse" && loaded.Accounts[1].Cookie==account.Cookie,"Provider settings did not roundtrip");
        loaded.SelectAccount(loaded.Accounts[0].Id);Require(loaded.Cookie==previous.Cookie && loaded.ModelId=="muse" && loaded.ApiKey==key,"Switch back changed Muse credentials or the proxy key");
        bool rejected=false;try{SettingsStore.ValidateProviderCookie(previous.Cookie,"gemini");}catch(ArgumentException){rejected=true;}
        Require(rejected,"Muse credential accepted as Gemini");
        rejected=false;try{SettingsStore.ValidateProviderCookie("__Secure-1PSID=","gemini");}catch(ArgumentException){rejected=true;}
        Require(rejected,"Empty Gemini credential accepted");
        using(var quota=new MuseQuotaClient()){
            rejected=false;try{await quota.FetchAccountAsync(account);}catch(ArgumentException){rejected=true;}
            Require(rejected,"Gemini cookie reached Muse quota client");
        }
        checks.Add("Provider migration, per-provider cookie/model/session isolation, stable proxy key and provider-specific quota");

        // Probe real embedded Gemini mode through a local account proxy. This
        // asserts no Muse bootstrap/cookie is used, without contacting Google.
        var proxy=new TcpListener(IPAddress.Loopback,0);proxy.Start();
        using var deadline=new CancellationTokenSource();
        var connect=Task.Run(async()=>{
            using var peer=await proxy.AcceptTcpClientAsync(deadline.Token);var stream=peer.GetStream();var data=new List<byte>();var one=new byte[1];
            while(data.Count<16384 && await stream.ReadAsync(one,deadline.Token)==1){data.Add(one[0]);if(data.Count>=4 && Encoding.ASCII.GetString(data.ToArray()).EndsWith("\r\n\r\n"))break;}
            await stream.WriteAsync("HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"u8.ToArray(),deadline.Token);
            return Encoding.ASCII.GetString(data.ToArray());
        });
        var probe=new TcpListener(IPAddress.Loopback,0);probe.Start();settings.Port=((IPEndPoint)probe.LocalEndpoint).Port;probe.Stop();
        account.ProxyUrl=$"http://127.0.0.1:{((IPEndPoint)proxy.LocalEndpoint).Port}";settings.NormalizeAccounts();
        using var server=new ServerController(root);
        try{
            await server.StartAsync(settings);
            settings.ProjectFolder=root;
            string shim=Path.Combine(root,"claude.ps1"),capture=Path.Combine(root,"gemini-launcher.json");
            File.WriteAllText(shim,"""
                $index=[Array]::IndexOf($args,'--model')
                @{model=$args[$index+1];main=$env:ANTHROPIC_MODEL;sub=$env:CLAUDE_CODE_SUBAGENT_MODEL;haiku=$env:ANTHROPIC_DEFAULT_HAIKU_MODEL}|ConvertTo-Json|Set-Content -LiteralPath $env:MUSE_PROVIDER_TEST_CAPTURE
                exit 0
                """);
            try{
                var launch=RuntimeFiles.ClaudeStartInfo(settings,server.RuntimeDirectory);launch.ArgumentList.Remove("-NoExit");
                launch.Environment["PATH"]=root+Path.PathSeparator+launch.Environment["PATH"];
                launch.Environment["MUSE_PROVIDER_TEST_CAPTURE"]=capture;
                // An inherited Muse override must not contaminate Gemini launch.
                launch.Environment["MUSE_MODEL"]="gemini";
                launch.CreateNoWindow=true;launch.RedirectStandardOutput=launch.RedirectStandardError=true;
                using var child=System.Diagnostics.Process.Start(launch)??throw new InvalidOperationException("Gemini launcher did not start");
                var stdout=child.StandardOutput.ReadToEndAsync();var stderr=child.StandardError.ReadToEndAsync();
                using var limit=new CancellationTokenSource(TimeSpan.FromSeconds(25));
                try{await child.WaitForExitAsync(limit.Token);}catch(OperationCanceledException){child.Kill(entireProcessTree:true);throw new InvalidOperationException("Gemini launcher timed out");}
                _=await stdout;_=await stderr;
                Require(child.ExitCode==0 && File.Exists(capture),"Gemini launcher failed");
                using var saved=JsonDocument.Parse(File.ReadAllText(capture));
                Require(new[]{"model","main","sub","haiku"}.All(key=>saved.RootElement.GetProperty(key).GetString()=="gemini"),"Gemini launcher retained a Muse model override");
            }finally{if(File.Exists(shim))File.Delete(shim);if(File.Exists(capture))File.Delete(capture);}
            using var http=new HttpClient(new HttpClientHandler{UseProxy=false});
            using var request=new HttpRequestMessage(HttpMethod.Post,$"http://127.0.0.1:{settings.Port}/v1/messages/count_tokens");
            request.Headers.Authorization=new System.Net.Http.Headers.AuthenticationHeaderValue("Bearer",key);
            request.Content=new StringContent("{\"model\":\"muse\",\"messages\":[{\"role\":\"user\",\"content\":\"check\"}]}",Encoding.UTF8,"application/json");
            using var response=await http.SendAsync(request);Require(response.StatusCode==HttpStatusCode.BadRequest,"Gemini rejected the original proxy key or accepted the Muse model");
            deadline.CancelAfter(TimeSpan.FromSeconds(8));
            rejected=false;try{await server.CheckMuseAsync();}catch(InvalidOperationException){rejected=true;}
            var observed=await connect;
            Require(rejected && observed.StartsWith("CONNECT gemini.google.com:443 ") && !observed.Contains("muse.ai") && !observed.Contains("hatch_sess") && !observed.Contains("fixture-gemini-secret"),"Gemini backend leaked cookie or connected to Muse");
        }finally{server.Stop();deadline.Cancel();proxy.Stop();}
        checks.Add("Embedded Gemini backend/launcher selects gemini for main/subagents, rejects Muse model and reaches only Google via account proxy");
    }
}
