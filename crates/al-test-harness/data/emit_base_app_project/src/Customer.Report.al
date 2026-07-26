report 50122 "Emitter Customer Report"
{
    Caption = 'Emitter Customer Report';
    UsageCategory = ReportsAndAnalysis;
    ApplicationArea = All;
    DefaultRenderingLayout = EmitterLayout;

    dataset
    {
        dataitem(Customer; Customer)
        {
            column(CustomerNo; "No.")
            {
            }
            column(CustomerName; Name)
            {
            }
            column(EmitterNote; "Emitter Note")
            {
            }
        }
    }

    rendering
    {
        layout(EmitterLayout)
        {
            Type = RDLC;
            LayoutFile = 'layout/EmitterCustomer.rdl';
            Caption = 'Emitter Customer Layout';
        }
    }
}
